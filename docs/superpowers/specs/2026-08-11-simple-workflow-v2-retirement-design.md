# Simple Brainstorm-to-Delivery and Workflow V2 Retirement Design

## Status

Direction approved on 2026-08-11.

This design makes the Simple brainstorm-to-delivery workflow the only writable
workflow path. Persisted manifest workflows remain historical read-only data
and may create an explicit Simple successor conversation, but they cannot be
continued or converted in place.

For brainstorm-to-delivery execution, this design supersedes the writable
manifest, gate-settlement, recovery, and completion-evidence behavior in:

- `2026-07-26-brainstorm-to-delivery-workflow-graph-design.md`;
- `2026-08-04-platform-generated-completion-evidence-design.md`; and
- `2026-08-09-completion-protocol-v2-only-design.md`.

Those documents remain useful descriptions of historical workflow data and its
read model. This design does not remove standalone delegation, continuation,
replacement lineage, conversation history, or historical workflow projection.

## Executive Decision

Codeg has one writable brainstorm-to-delivery mode: **Simple**.

```text
new brainstorm-to-delivery root
  -> no workflow manifest
  -> Implementation Plan defines task structure
  -> per-workflow progress.md records orchestration progress
  -> durable delegation runs provide live child lifecycle
  -> parent adjudicates reviews and repository evidence
```

Any conversation that owns or is bound to a persisted manifest workflow is an
**archived workflow**. It remains viewable, but all root prompts, child prompts,
workflow writes, and workflow-bound delegation admissions fail before side
effects. The user can explicitly create a new Simple conversation in the same
workspace from the archived workflow's Plan.

There is no automatic fallback, in-place conversion, dual write, or mid-run
mode switch. Simple never publishes a shadow manifest. Archived workflow state
never controls a Simple successor.

## Motivation and Incident Evidence

The manifest workflow attempted to make gate and completion state durable, but
it also created several independently mutable authorities:

- Implementation Plan and progress ledger;
- manifest revision and workflow state;
- gate settlements and lineages;
- node and run bindings;
- completion evidence and artifact recovery; and
- root-only recovery authorization.

In conversation 3635, sub-agent processes did start successfully. The blocking
sequence was semantic state disagreement:

```text
verification-only Task produced no commit
  -> completion required a producer commit
  -> completion artifact could not be repaired from the root MCP surface
  -> an approved manifest was republished as estimated
  -> the Plan gate reopened
  -> A8.3 rejected the next Task
  -> no usable pre-admission authorization path remained
```

The Simple design removes that failure class by eliminating platform-owned
workflow gates and completion evidence from admission. It deliberately accepts
weaker formal consistency in exchange for fewer synchronized state surfaces,
more direct recovery, and a workflow that can continue when display metadata is
stale.

## Goals

- Make Simple the only mode that can start new brainstorm-to-delivery work.
- Prevent every persisted manifest workflow from producing new semantic state.
- Keep archived workflow transcripts, graphs, runs, reports, and evidence
  readable.
- Let a user create or reopen exactly one Simple successor for an archived
  workflow.
- Build the Simple workflow display from the Plan, progress ledger, and durable
  delegation lifecycle without turning display disagreement into admission
  failure.
- Preserve generic delegation continuation and replacement safeguards.
- Keep desktop and server behavior identical.
- Retire v2 mutation code in stages without a destructive database migration.

## Non-Goals

- Proving Task completion with a platform gate or cryptographic evidence graph.
- Importing archived gate approvals, completion Cards, reviewer settlements,
  task IDs, or lineage counters into Simple.
- Rewriting historical workflow rows to look like Simple rows.
- Automatically creating a successor during startup, recovery, or prompt send.
- Deleting historical workflows, conversations, reports, or database tables.
- Changing standalone sub-agent delegation outside brainstorm-to-delivery.
- Making a missing or malformed display ledger block code execution.
- Preserving every internal v2 write API after the retirement window.

## Terminology

**Simple root** is a root conversation with no `delegation_workflows` header
and either a `simple_workflows` descriptor or recognized Simple A1 delegation
history. The descriptor is the preferred display locator, not a prerequisite
for generic delegation.

**Archived workflow** is a conversation that owns a persisted workflow header,
or a child conversation with a durable run binding to one. This includes
historical v1 and v2 rows; existing v1-specific read-only errors may remain,
while v2 retirement uses the stable `workflow_v2_retired` code.

**Simple successor** is a new root conversation created by an explicit user
action from an archived workflow. It shares the workspace, not workflow
semantics.

**Descriptor** is locator metadata for a Simple root: Plan path, progress path,
and optional archived source identity. It contains no Tasks, gates, decisions,
digests, revisions, or completion state.

**Declared progress** is the parent-maintained state in the structured block of
the Simple progress ledger. It is operational context, not a platform gate.

## System Invariants

1. A conversation cannot own both a Simple descriptor and a workflow header.
2. New production code cannot create a workflow header or manifest.
3. An archived workflow cannot accept root prompts, child follow-ups,
   delegation, continuation, replacement, settlement, recovery, or completion
   mutation.
4. A Simple workflow never reads archived semantic state as an authorization.
5. Creating a successor copies document locations and workspace identity only.
6. Plan structure, declared progress, and durable run lifecycle may disagree;
   disagreement produces a visible reconciliation warning, never a workflow
   admission error.
7. Generic delegation budgets and lineage rules remain authoritative for the
   individual run operations they protect.
8. Explicit conversation deletion remains allowed for both modes.
9. Infrastructure may terminalize abandoned processes or run transport state,
   but it cannot reduce a gate or write semantic completion for an archived
   workflow.
10. Once a root is enrolled as Simple or owns an archived workflow identity,
    its mode is frozen. An ordinary pre-enrollment draft may become Simple by
    registration or its first recognized A1 run; it can never later acquire a
    workflow header. Any other mode change requires a new root conversation.

## Mode Resolution

The backend resolves conversation behavior from durable identity, not a client
flag:

| Durable identity | Mode | Writable behavior |
| --- | --- | --- |
| Owns or is bound to a workflow header | Archived workflow | Read-only |
| Owns a Simple descriptor and no workflow header | Simple, registered | Simple orchestration |
| Has recognized A1 runs and no workflow header | Simple, observed compatibility | Simple orchestration |
| Neither | Ordinary conversation | Existing non-workflow behavior |
| Workflow header plus Simple descriptor | Corrupt identity | Fail closed and report an invariant violation |

The UI may cache this projection but cannot select or override it.

## Simple Descriptor

Add one locator-only table:

```text
simple_workflows
  parent_conversation_id  INTEGER PRIMARY KEY REFERENCES conversations(id)
  plan_rel_path           TEXT NOT NULL
  progress_rel_path       TEXT NOT NULL
  source_workflow_id      TEXT NULL UNIQUE REFERENCES delegation_workflows(workflow_id)
  created_at              TIMESTAMP NOT NULL
  updated_at              TIMESTAMP NOT NULL
```

Deleting the Simple conversation deletes its descriptor. The unique source
constraint gives an archived workflow at most one live successor. If that
successor is explicitly deleted, the descriptor and link disappear and the
user may create another successor.

`plan_rel_path` and `progress_rel_path` are normalized workspace-relative paths.
They may be updated as locators, but updating them does not settle, reopen, or
authorize anything. Descriptor creation or update is rejected if the
conversation already owns a workflow header.

A new Simple workflow registers its descriptor after the Plan path is known.
Registration is idempotent and returns the recommended isolated progress path:

```text
.superpowers/sdd/<parent-conversation-id>/progress.md
```

Descriptor registration improves display discovery but is not an execution
gate. If registration or projection is unavailable, the parent can still use
the Plan, ledger, and generic delegation tools through the no-manifest
compatibility path. Recognized A1 runs make that root observable as an
unregistered Simple workflow, and the UI falls back to the existing Plan and
Sessions surfaces until registration succeeds.

## Simple Skill Contract

The brainstorm-to-delivery Skill is rewritten from the simpler behavior at
commit `99ddba923112cf82f9bde1dd5b8455a691133c0d`, but it is not restored as an
unreviewed byte-for-byte copy. Later generic fixes for tool discovery,
continuation, replacement, workspace safety, and report recovery remain where
they do not depend on workflow v2.

The Skill must:

- never call workflow capability, manifest, settlement, workflow recovery, or
  completion-evidence tools;
- use `writing-plans` to produce the Implementation Plan;
- register the Simple Plan and progress paths after the Plan exists;
- maintain the progress ledger before dispatch and after every observed state
  change;
- execute implementation Tasks serially;
- use generic `delegate_to_agent` for a work unit's first run and
  `continue_delegation` for a valid continuation;
- keep stable `work_unit_key`, child conversation ID, latest task ID, role,
  agent, profile, recovery count, and replacement metadata in progress;
- preserve independent implementation and review roles required by the Skill;
- adjudicate document and code review in the parent from reports and repository
  facts, without publishing a gate settlement;
- re-inspect disk and rerun covering checks after compaction or interrupted
  execution; and
- complete delivery from repository state, verification results, and final
  review rather than a platform completion Card.

Plan or progress parsing failure must never instruct the parent to bypass
generic delegation identity, budget, or replacement errors. Simple removes
workflow v2, not the underlying run-safety rails.

## Plan and Progress Formats

### Implementation Plan

The existing `writing-plans` format remains the source of task structure. The
backend parses Markdown with `pulldown-cmark` and recognizes task headings of
the form `Task <positive integer>: <title>` at heading level 2 or 3. The
standard generated form remains:

```markdown
### Task 1: Add the parser
```

The parser ignores headings inside code blocks and rejects duplicate Task
indices. Non-contiguous indices and malformed headings produce projection
warnings rather than execution blocks. Existing bounded Plan-material parsing
should be extracted or reused instead of introducing a second ad hoc Markdown
scanner.

The Plan supplies stable display order, Task index, title, body, declared file
touchpoints, and verification text. It does not supply a gate state.

### Progress ledger

The progress ledger stays human-readable and contains exactly one bounded JSON
block marked as follows:

```text
<!-- codeg-simple-progress-v1
{
  "schema_version": 1,
  "plan_rel_path": "docs/superpowers/plans/example.md",
  "active_task_index": 2,
  "tasks": [
    {
      "index": 1,
      "status": "completed",
      "commit": "0123456789abcdef",
      "runs": [
        {
          "role": "implementer",
          "agent_type": "grok",
          "task_id": "task-id",
          "child_conversation_id": 42,
          "state": "completed"
        }
      ]
    },
    {
      "index": 2,
      "status": "in_progress",
      "runs": []
    }
  ],
  "final_review_status": "pending",
  "updated_at": "2026-08-11T00:00:00Z"
}
-->
```

Allowed Task statuses are `pending`, `in_progress`, `completed`, and `blocked`.
Allowed run states are display-oriented mirrors of durable delegation states;
unknown values are preserved as warnings rather than coerced to success.

The structured block is the parseable snapshot. Notes, findings, commands, and
recovery history may follow it as normal Markdown. The parent replaces the one
structured block atomically and appends or edits human notes as needed. The
block is bounded to 64 KiB, the full ledger to 512 KiB, and the Plan to the
existing Plan-material size limits.

## Simple Display Projection

The backend projects a Simple graph by joining three sources:

1. Plan tasks define structure and order.
2. The structured progress block defines declared orchestration status and run
   references.
3. Durable delegation rows provide actual child lifecycle, timing, activity,
   and conversation links for referenced task IDs and recognized work-unit
   keys.

The existing `WorkflowGraphSnapshot` transport may be extended with
`compatibility: "simple"`. A Simple snapshot has no workflow ID, manifest
revision, graph revision, manifest state, workflow completion projection, or
gates. It may reuse phase, node, edge, runtime-stat, and child-open fields.
Simple adds `pending` and `in_progress` where the existing lifecycle vocabulary
cannot express declared progress without gate terminology. Snapshot and node
DTOs also carry bounded `projection_warning_codes`; node DTOs carry
`sync_state: "in_sync" | "out_of_sync"`. Synchronization is orthogonal to the
node lifecycle status.

Projection rules are deliberately non-blocking:

- A Plan Task without progress is `pending`.
- Declared progress supplies the visible Task state.
- A matching active durable run adds live activity even if the ledger has not
  yet been refreshed.
- Durable terminal state enriches the row but does not silently claim that the
  parent completed review or verification.
- A progress task absent from the Plan, a task-ID mismatch, a failed run marked
  completed, a missing commit, or a stale Plan path sets the row's sync state
  to `out_of_sync` while preserving its lifecycle status.
- Malformed or missing files return the largest safe partial projection plus a
  warning. They do not reject delegation.

Simple overall state is deterministic display data: `blocked` when declared
progress is blocked, `completed` only when every Plan Task and final review are
declared complete, `in_progress` after any work has started, and `pending`
otherwise. Projection warnings do not change overall state.

The UI labels this projection as Simple and presents reconciliation warnings in
the graph or progress surface. It must not imply that a display warning is a
platform gate.

### Refresh model

Simple has no persisted graph clock. Descriptor creation or locator updates
emit a conversation-scoped refresh event. The existing workspace file-change
stream invalidates snapshots when the exact Plan or progress path changes, and
durable delegation lifecycle events invalidate snapshots for the parent.

Refreshes are debounced and re-read bounded files from disk. Opening the
conversation or expanding the overlay always performs a fresh read; while the
overlay remains visible, the existing bounded refresh fallback covers missed
filesystem events. The frontend uses a local request generation so an older
response cannot overwrite a newer projection. None of these refresh mechanisms
writes workflow semantics.

## Archived Workflow Read-Only Boundary

One shared backend guard detects a workflow-owning root or workflow-bound child
before any semantic side effect. For v2 it returns:

```text
code: workflow_v2_retired
message: This workflow is archived and read-only. Continue in a Simple successor.
successor_conversation_id: <id when present>
can_create_simple_successor: <true when no successor exists and Plan is available>
```

The guard applies to:

- foreground, automation, and chat-channel root prompt admission;
- prompts sent directly to workflow-bound child conversations;
- first delegation, continuation, and replacement;
- manifest publication and gate settlement;
- workflow and delegation recovery authorization that targets a bound run;
- workflow recovery;
- completion submission, retry, adjudication, and artifact repair;
- Final delivery mutation and automatic root wake; and
- any internal call path capable of semantic gate or completion writes.

The check occurs before prompt enqueue, transcript append, budget consumption,
run reservation, child creation, authorization insertion, attention creation,
or workflow transaction. Hiding UI controls is not sufficient.

Read operations remain available:

- conversation and transcript loading;
- workflow graph and state snapshots;
- child conversation and report opening;
- recorded gates, Cards, evidence, revisions, and runtime statistics;
- source/successor navigation; and
- explicit conversation deletion.

Creating or deleting a Simple successor descriptor is permitted navigation
metadata stored outside the archived workflow tables. It does not mutate the
archived manifest, gates, nodes, runs, evidence, or completion state.

If an application upgrade encounters an already running workflow child, normal
process supervision may record transport termination or close an abandoned run.
It must not parse new semantic completion, reduce a gate, advance a node, or
wake the archived root.

## Workflow Tool Retirement

New root MCP catalogs no longer expose workflow mutation tools. Read-only graph
projection remains an application API concern, not a reason to expose mutation
capabilities to the agent.

Direct calls from stale clients or already connected sessions still pass
through the archived-workflow guard. Removing a tool from the catalog is not a
substitute for server-side rejection.

An attempted manifest publication is rejected with `workflow_v2_retired` even
when the caller has no existing workflow header. This closes the new-workflow
creation path rather than applying the guard only to archived rows.

Generic delegation tools remain available for Simple and ordinary roots.
Recognized A1 keys without a manifest continue through the existing
compatibility path and never create a workflow header as a side effect.

## Creating a Simple Successor

The archived workflow overlay displays a primary command named **Continue in
Simple**. The command is available only when the current user can access the
source conversation and its workspace.

The backend operation is idempotent and accepts a client request token. It:

1. loads and verifies the archived source workflow;
2. resolves the active persisted `plan_target_rel_path` as a normalized path
   inside the source workspace;
3. requires the Plan file to exist and satisfy bounded UTF-8 reading;
4. returns the existing linked successor when one already exists;
5. otherwise creates a new root conversation in the same folder with the
   source root's agent type and delegation route override;
6. creates its Simple descriptor with a new isolated progress path and the
   immutable `source_workflow_id` link;
7. opens the new conversation; and
8. admits one explicit bootstrap prompt caused by the user's click.

Conversation creation and descriptor/link insertion are atomic. Prompt
admission occurs only after that transaction commits. Concurrent double-clicks
converge on the unique source link and open the same successor.

The successor title is the source title plus a localized Simple suffix. The
bootstrap prompt contains only:

- the fact that this is a Simple successor;
- source conversation ID for navigation;
- workspace-relative Design and Plan paths when available;
- the new progress path;
- an instruction to inspect Git and the filesystem before reconstructing
  progress; and
- an instruction never to import archived workflow semantics.

It does not copy or present as authority:

- workflow ID, manifest state, revision, gate, or node ID;
- approval or completion outcome;
- reviewer evidence or artifact digest;
- archived task ID or child conversation ID;
- continue or replacement counters; or
- an assertion that any Task is complete.

The archived transcript and graph remain linked for manual inspection. They are
not injected wholesale into the successor prompt.

If the Plan path is missing, outside the workspace, unreadable, oversized, or
invalid UTF-8, successor creation returns
`simple_successor_plan_unavailable` without creating a conversation. The user
can repair the Plan or start an ordinary Simple workflow manually.

## Successor Progress Reconstruction

The first Simple turn reconstructs progress from repository facts:

1. Parse the current Plan from disk.
2. Inspect branch, HEAD, status, relevant diffs, and Plan touchpoints.
3. Read archived progress only as a navigation hint when useful.
4. For each apparently completed Task, identify the implementing changes and
   rerun or validate the Task's covering checks.
5. Mark a Task `completed` only when current repository evidence supports it.
6. Mark uncertain work `pending` or `blocked` with a concise reason.
7. Write the new Simple progress block before dispatching more work.

An existing clean commit may satisfy a Task; Simple does not require every
reconstructed Task to create a new commit. A dirty worktree follows the normal
workspace safety gate and is never reset, stashed, committed, or discarded
without user authorization.

## Error Handling

| Condition | Behavior |
| --- | --- |
| Archived root or child receives a prompt | Reject with `workflow_v2_retired` before side effects |
| Archived workflow mutation is called directly | Reject with `workflow_v2_retired` |
| Any caller attempts to create a new manifest workflow | Reject with `workflow_v2_retired` |
| Successor already exists | Return and open the existing successor |
| Archived Plan is unavailable | Return `simple_successor_plan_unavailable`; create nothing |
| Simple descriptor conflicts with workflow header | Fail closed with `workflow_mode_conflict` |
| Simple Plan cannot be parsed | Show partial/fallback UI; execution remains available |
| Progress block is absent or malformed | Show Plan tasks as pending plus a warning |
| Progress and durable run disagree | Set `sync_state=out_of_sync`; do not mutate or block automatically |
| Generic continuation/replacement is invalid | Preserve the existing typed delegation error |
| Descriptor registration fails | Keep execution available and fall back to Plan/Sessions UI |

## Security and Bounds

- Every Plan and progress path is normalized and resolved beneath the
  conversation's workspace; absolute paths and traversal are rejected.
- Desktop and server commands derive conversation, folder, and source workflow
  identity from the database rather than trusting client-supplied links.
- Server mode applies its normal authentication before successor creation or
  archived history reads.
- File reads use explicit byte limits and UTF-8 validation.
- Parsed Markdown and progress notes are rendered as data and cannot invoke
  tools or commands.
- Successor bootstrap text is constructed by the backend from bounded fields;
  archived transcript prose is not copied into hidden instructions.
- Error DTOs expose conversation IDs and relative paths only to an authorized
  caller and never expose absolute workspace paths.

## Rollout and Removal

### Phase 1: Behavioral retirement

- Rewrite the Skill to Simple.
- Prevent creation of new workflow headers and manifests.
- Install the archived-workflow read-only guard on every write surface.
- Add Simple descriptors, Plan/progress projection, and successor creation.
- Keep all v2 tables, entities, read DTOs, graph projection, and historical UI.

### Phase 2: Remove production mutation surfaces

After at least one stable release in which the shipped Skill makes no v2 write
attempts and successor creation has no unresolved critical regression:

- remove workflow mutation tools from schemas and routing;
- remove automatic workflow wake and write-only UI controls;
- delete production constructors and callers that can publish or settle v2;
- retain typed rejection at stale transport boundaries; and
- retain fixtures needed to prove historical read compatibility.

### Phase 3: Internal code retirement

After the read model no longer imports mutation-only code:

- delete gate reduction, completion mutation, workflow recovery, and admission
  implementation that has no historical projection dependency;
- keep migrations, SeaORM values needed to deserialize rows, read projectors,
  and explicit deletion support; and
- do not drop historical tables merely to finish code cleanup.

There is no permanent feature flag that allows new v2 workflows. Rollout phases
control code removal, not user-selectable execution modes.

## Testing Strategy

### Backend mode and mutation fences

- New Simple registration creates no `delegation_workflows` row.
- Manifest publication without an existing workflow header is rejected and
  creates no workflow row.
- Every archived root and child prompt path rejects before transcript, process,
  budget, task, authorization, attention, or semantic workflow mutation.
- Every public workflow mutation endpoint and MCP path returns the stable
  retirement error.
- A1 delegation without a manifest still admits through generic delegation.
- Ordinary conversations and standalone delegation remain unchanged.
- Operational cleanup of an active archived run writes no gate or completion
  evidence.

### Successor creation

- Desktop and server commands create the same descriptor and link.
- Concurrent identical requests create exactly one successor.
- Repeated requests open the existing successor.
- A deleted successor permits explicit recreation.
- Source workspace, agent type, and route override are preserved.
- Missing, traversal, absolute, oversized, and invalid UTF-8 Plan paths create
  no partial conversation or descriptor.
- The bootstrap contains paths and reconstruction instructions but none of the
  forbidden archived semantic fields.

### Plan and progress projection

- Parse standard level-2 and level-3 Task headings with multi-digit indices.
- Ignore code-block, quoted, and list text that only resembles Task headings.
- Bound duplicate, non-contiguous, missing, oversized, and invalid UTF-8 input.
- Parse the exact structured progress marker with strict JSON fields and bounds.
- Join task IDs only to durable runs owned by the Simple parent.
- Mark failed-completed, missing-task, stale-path, and foreign-task conflicts
  with `sync_state=out_of_sync` without rejecting execution.
- Discard stale refresh responses by local request generation and refresh on
  descriptor, exact-file, delegation, activation, and visible-overlay signals.
- Preserve runtime statistics and child-open actions for matched runs.

### Frontend

- Archived graphs remain readable and show a read-only banner.
- Archived mutation controls are absent.
- Continue in Simple creates or opens the linked successor.
- Button loading and error states do not create duplicate conversations.
- Simple graphs show Plan order, declared status, live run activity, and
  reconciliation warnings without gate language.
- Missing descriptor or malformed files fall back to existing Plan and Sessions
  surfaces.
- All new copy is present in every supported locale.

### End-to-end regressions

- Start a new brainstorm-to-delivery session, produce a Plan, register Simple,
  execute and review Tasks, compact the parent context, recover from progress,
  and deliver without any workflow header.
- Open a historical v2 workflow, verify every write path is rejected, create a
  Simple successor, reconstruct partial work, and continue with a fresh
  delegation lineage.
- Run both flows in desktop and server mode.

## Success Criteria

- No new production brainstorm-to-delivery session creates a manifest workflow.
- Historical v2 workflows remain fully viewable but cannot produce semantic
  writes through any supported transport.
- A user can create or reopen one Simple successor from an archived workflow in
  one explicit action.
- The successor imports no archived approvals, task IDs, gates, completion
  outcomes, or recovery counters.
- Simple display survives stale or malformed progress without blocking Task
  execution.
- Generic delegation identity, continuation, replacement, and budget errors
  remain enforced.
- The exact `approved -> estimated -> Plan gate reopened` failure class cannot
  occur in Simple because those states do not exist.
- V2 mutation code can be removed incrementally without deleting historical
  data or breaking archived graph reads.

## Alternatives Considered

### Keep Simple and v2 as permanent writable modes

Rejected. It doubles the execution and test matrix, requires users and agents
to understand two authorities, and creates pressure for unsafe fallback between
them. The practical value of Simple is lost if v2 state can still block or
reinterpret it.

### Delete all v2 code and data immediately

Rejected. Historical graphs, reports, conversations, and diagnostics still have
value, while a destructive migration would couple retirement to many foreign
keys and read projections. Behavioral retirement provides the stability gain
without that migration risk.

### Silently fall back from a stuck v2 workflow to Simple in place

Rejected. The same conversation would then contain incompatible task IDs,
review evidence, recovery budgets, and completion claims. A new successor is a
clear trust boundary and makes progress reconstruction explicit.

### Copy archived v2 progress into the successor

Rejected. Existing ledgers contain manifest revisions, gates, task IDs, and
claims that may already disagree with the repository. The successor may inspect
them as hints but writes a new ledger from current repository evidence.

### Drive Simple display only from transcript events

Rejected as the primary design. Transcript `plan`, TodoWrite, and reasoning
events are useful fallbacks, but they do not reliably survive agent differences,
compaction, or file-based Plan revision. The Plan and per-workflow progress
ledger are the intended Simple projection sources.
