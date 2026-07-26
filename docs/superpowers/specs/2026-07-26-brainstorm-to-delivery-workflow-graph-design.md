# Brainstorm-to-Delivery Workflow Graph Design

Date: 2026-07-26

Status: Design approved 2026-07-26 after document review (KimiK3, Opus4.8,
Grok, Codex) and Contract Amendments A1–A18 + B1–B14.

## Summary

Add a workflow-aware Graph view to the existing sub-agent overlay for
`brainstorm-to-delivery` conversations. The selected UX is a compact phase
overview inside the conversation with an on-demand expanded Graph. The current
session list remains available through a `Workflow / Sessions` segmented
control and remains the only view for unrecognized or native delegation.

The Graph is plan-driven rather than transcript-driven. A versioned, durable
workflow manifest defines the complete estimated task chain as soon as the
implementation plan is written. Durable delegation runs then light up stable
work-unit nodes without moving the planned layout. Continue runs and legal
replacement sessions remain inside the same work-unit node.

Document review groups are represented as fan-out/fan-in gates. Every reviewer
has an independent work-unit node and thread lineage. The gate advances only
after all required reviewers return and the parent conversation explicitly
records its evidence-based adjudication. Reviewer verdicts are not votes.

Historical conversations load a backend-projected `WorkflowGraphSnapshot`
from the persisted manifest, gate settlements, and `delegation_task_runs`.
Conversation text is never parsed to reconstruct workflow state. Older
conversations degrade to an observed-only Graph when recognized work-unit keys
exist, or to the existing session list when they do not.

## Current State And Evidence

The repository already has several foundations this design should extend:

- `src/components/chat/sub-agent-overlay.tsx` displays every Codeg and native
  delegation in a resizable overlay. Codeg sources are grouped by durable child
  conversation identity, and the latest generation drives the visible row.
- The overlay already exposes run count, replacement state, status, runtime
  statistics, touched files, and an action that opens the child conversation in
  a main tab.
- `delegation_task_runs` persists `generation`, `lineage_root_task_id`,
  `work_unit_key`, `replaced_task_id`, `replacement_reason`, profile, agent,
  status, card summary, runtime statistics, and child conversation identity.
- `DelegationRunSnapshot` intentionally exposes immutable per-run card state,
  but it does not expose `work_unit_key`, `lineage_root_task_id`, or a workflow
  identity to the frontend.
- `get_folder_conversation_core` already recovers historical child bindings and
  all durable run snapshots before returning `DbConversationDetail`, then
  injects run metadata into historical tool-call blocks. Historical first
  render attaches `workflow_graph` on this same shared `_core` path (not a
  separate transcript parse).
- `brainstorm-to-delivery` defines stable work-unit-key materials for Design,
  Plan, Task implementer, Task reviewer, and final review work units. It also
  defines thread reuse, replacement, and recovery rules.

Those records can explain work that has already happened. They cannot describe
future Tasks before delegation, identify the complete parallel review group,
or persist the parent's adjudication of conflicting reviewer findings. A new
workflow manifest and gate-settlement contract are therefore required.

## Goals

- Show the current delivery phase at a glance without leaving the conversation.
- Publish the complete estimated Task implementation/review chain immediately
  after the implementation plan is written.
- Keep planned node positions stable while execution lights up their states.
- Group every continue run and replacement generation under its stable work
  unit.
- Represent concurrent Design and Plan reviewers as parallel branches that
  converge on an explicit parent-adjudication gate.
- Preserve the existing session list, row details, and open-child action.
- Restore the same Graph after closing and reopening a historical conversation.
- Give desktop and server mode the same persistence, API, event, and projection
  behavior.
- Preserve legacy conversations without guessing workflow stages from prose.
- Keep internal work-unit keys, absolute document paths, prompts, and route
  details out of frontend DTOs.

## Non-Goals

- Building a general-purpose graph editor or arbitrary workflow engine.
- Visualizing every Codeg delegation or native sub-agent as a workflow.
- Parsing implementation-plan Markdown or chat transcripts into Graph nodes.
- Replacing message-stream delegation cards.
- Changing Grok/Codex role routing, SDD review requirements, recovery budgets,
  or replacement eligibility.
- Adding parallel Task implementation. Task implementation remains serial under
  `brainstorm-to-delivery`.
- Providing historical time travel through every manifest revision in v1. The
  database retains revisions for audit, while the UI renders the latest active
  projection plus retained observed work.
- Modifying the generic `writing-plans` or `subagent-driven-development` Skill
  contract in v1.

## Terms

**Workflow** is one durable `brainstorm-to-delivery` orchestration instance
owned by a parent conversation.

**Manifest** is a bounded, versioned document describing phases, stable nodes,
dependencies, review gates, expected agent/profile roles, and internal
work-unit bindings.

**Phase** is one high-level compact step: Design, Plan, Tasks, or Final.

**Work unit** is the reusable thread identity defined by one stable
`work_unit_key`. It is the Graph node granularity for delegated work.

**Run** is one generation in `delegation_task_runs`. Initial delegation,
continue, and replacement runs are displayed inside their work-unit node.

**Document gate** is a Design or Plan synchronization point. A concurrent
document gate has multiple required reviewer nodes and requires explicit
parent adjudication. A conditional Design review that does not dispatch an
independent reviewer still uses a zero-reviewer parent-acknowledgement gate, so
its completion is durable rather than inferred from chat.

**Execution gate** is a derived Task or Final synchronization point. It passes
from durable implementer/reviewer runs and validated card summaries; it is not
settled manually by the parent and has no gate-settlement row.

**Estimated node** is defined by the manifest but has no admitted delegation
run yet.

**Observed-only Graph** is a compatibility projection built from recognized
durable work-unit keys when no manifest exists. It never invents unobserved
future nodes.

## Durable Invariants

1. A workflow Graph is shown only for a recognized, structured workflow.
2. Chat text, task previews, model output, and Markdown are never authoritative
   workflow structure.
3. The latest accepted manifest defines planned structure. Durable run and gate
   records define actual state.
4. Durable actual state always overrides an estimated manifest state.
5. One work unit is one visible delegated-work node, regardless of run count or
   replacement generation.
6. Publishing a new manifest revision cannot erase or change the identity of a
   started or completed work unit.
7. A plan revision may atomically replace only unstarted estimated nodes.
8. A document gate is not complete until all required reviewers have returned
   and the parent explicitly settles the gate. A zero-reviewer Design gate
   requires the same explicit parent settlement after self-review.
9. Reviewer findings are adjudicated by evidence, not majority vote or reviewer
   priority.
10. A required reviewer run without a validated terminal card summary cannot
    pass its document or execution gate.
11. The frontend never receives raw `work_unit_key`, absolute plan/design path,
    prompt body, route fingerprint, or launch configuration.
12. Manifest publication is a hard gate only after the backend advertises the
    complete v1 capability.
13. A consistently absent capability selects the legacy Sessions/observed-only
    path. A present, inconsistent, or failing capability blocks the workflow
    rather than silently degrading.

## Selected User Experience

### Compact overlay

When a recognized workflow snapshot exists, the sub-agent overlay defaults to
the Workflow segment. Its compact state shows:

- the workflow label and overall state;
- a four-step `Design -> Plan -> Tasks -> Final` phase rail;
- completed, current, blocked, and pending phase styling;
- the current work item, role/agent, run state, and round count;
- Task position when the approved manifest supplies a total, for example
  `Task 2 / 5`;
- aggregate concurrent-review progress such as `Plan review 2 / 3`;
- an expand icon for the full Graph;
- a `Workflow / Sessions` segmented control.

The Sessions segment renders the current Codeg/native row list without
changing its existing behavior. Conversations without a recognized workflow
open directly in Sessions and do not show a disabled or empty Workflow segment.

### Expanded Graph

Expansion stays anchored to the conversation instead of opening a separate
main tab. It uses a larger responsive panel that preserves chat context. The
Graph uses deterministic phase lanes and Task rows, not a force-directed
layout and not a draggable canvas.

The expanded view shows:

- Design and Plan review groups;
- one implementer and one independent reviewer work unit per planned Task;
- the final independent review work unit;
- dependency edges from the manifest;
- review/fix/re-review loops without creating a new work-unit node;
- completed, running, blocked, failed, canceled, estimated, and superseded
  states with text or shape in addition to color;
- run count and replacement indication inside work-unit nodes;
- reviewer/profile identity where it disambiguates parallel branches;
- a selected-node detail line with the latest status and session action.

Clicking an observed work-unit node opens its latest child conversation using
the existing delegated-child tab path. Estimated nodes have no session action.
Replacement history remains discoverable from node detail, while the default
open action targets the latest valid child conversation.

### Stable estimated chain

The manifest revision published immediately after plan creation includes the
full ordered Task chain, implementer/reviewer pairs, dependencies, and final
review. These nodes render as estimated before implementation begins.

Execution changes state but not layout:

- an admitted run changes its work unit from estimated to reserving/running;
- terminal run and card-summary state changes it to completed, changes
  requested, blocked, failed, or canceled;
- continue increments round count and keeps the node identity;
- replacement increments replacement history and keeps the work-unit node;
- the next dependency-ready node becomes current when no run is active;
- the final phase lights only after every Task gate passes.

No percentage is displayed unless the active manifest provides a complete Task
count. A skeleton or observed-only Graph uses phase/current labels instead of a
fabricated completion percentage.

### Concurrent document review

Design and Plan review groups render as one compact phase gate and fan out in
the expanded Graph:

```text
document revision
  -> reviewer/profile A work unit
  -> reviewer/profile B work unit
  -> reviewer/profile C work unit
  -> parent adjudication gate
  -> next phase OR document revision loop
```

Every reviewer/profile has a separate stable work-unit key and run history.
The compact phase shows returned/required, running, and blocked counts. The
expanded graph shows individual verdict summary and round count.

The gate waits for all required reviewers. The parent deduplicates findings,
checks repository evidence, fixes every valid Critical or Important issue, and
then records one explicit gate outcome. Valid issues create a document-revision
loop followed by continue on the same reviewer threads. Minor findings are
fixed or recorded according to the Skill contract.

Optional document reviewers are allowed only in Design and Plan gates. Task and
final review remain Codex-only as required by `brainstorm-to-delivery`.

When the Skill's existing conditional Design-review rule decides no independent
review is required, the manifest contains no Design reviewer work units. The
parent settles that Design gate as approved after its own required self-review.
Plan review always contains at least the required Codex reviewer and follows the
normal fan-out/adjudication path.

### Responsive behavior

- The compact four-phase rail remains fixed and readable at narrow widths.
- Expanded desktop layout uses phase columns and vertically stacked Task rows.
- Narrow layouts switch to a vertical dependency flow; they do not shrink text
  or require a free-pan canvas.
- Long workflows virtualize or progressively mount Task rows while preserving
  deterministic order and a stable selected node.
- Labels wrap within bounded nodes. They do not resize the graph geometry.
- Keyboard focus follows dependency order, and every observed node exposes an
  accessible name containing phase, Task, role, agent, and status.

## Capability And Skill Contract

### Capability negotiation

The Codeg MCP companion exposes `get_workflow_capabilities` on new companions.
It advertises `workflow_manifest_v1` only when both v1 mutation tools and every
required persistence path are enabled. Tool-catalog presence and the returned
capability value must agree.

Capability discovery has four explicit outcomes:

- capability tool and all v1 workflow tools absent: legacy companion;
- capability tool returns v1 false and workflow mutation/recovery tools absent:
  legacy mode on a new companion;
- capability tool returns v1 true and all required v1 tools are present
  (`get_workflow_state`, `publish_workflow_manifest`, `settle_workflow_gate`):
  v1 mode (see Contract Amendments B9);
- every other combination: inconsistent companion, which hard-blocks before
  workflow mutation or review dispatch.

A capability-tool call or response-validation failure is also a hard block. It
cannot be interpreted as a legacy companion because the new contract was
already advertised.

The updated `brainstorm-to-delivery` Skill follows this rule:

- capability absent: record legacy mode and continue the existing workflow;
  the UI keeps the session list or observed-only compatibility projection;
- capability present: every required manifest/gate operation is a hard gate;
  validation, ownership, persistence, or authorization failure pauses the
  workflow and reports the typed error;
- capability present but one required tool missing: treat as an inconsistent
  capability and hard-block, not as legacy mode.

This preserves old Codeg compatibility without making a new Codeg silently
lose its promised workflow state.

### Manifest lifecycle in `brainstorm-to-delivery`

The repository Skill text must add these mandatory steps:

1. At workflow entry, discover capability before the first conditional Design
   review dispatch.
2. When v1 is present, publish a skeleton manifest containing workflow identity,
   Design/Plan reviewer groups known from the prompt, high-level phase order,
   and Task/Final placeholders. Store the returned workflow id and revisions in
   the SDD progress ledger.
3. After `writing-plans` writes the implementation plan and before Plan review
   dispatch, publish an estimated manifest revision containing the complete
   Task chain and final-review node.
4. After every material plan revision and before its re-review, publish a new
   estimated revision with optimistic concurrency.
5. After reviewer results return, explicitly settle each concurrent Design or
   Plan gate with the parent's adjudicated result.
6. Mark the plan manifest approved only after its complete document-review gate
   passes.
7. During SDD, require every delegated work-unit key to match the approved
   manifest node for that Task/role/profile.
8. On compaction or recovery, load the durable workflow and ledger first. Do not
   recreate a workflow or replay a manifest sequence from memory.
9. Include workflow id, manifest revision, graph revision, capability mode, and
   latest gate settlement in the progress ledger.

The backend derives run state automatically. The Skill does not manually mark
implementer/reviewer nodes running or completed.

The generic `writing-plans` Skill remains responsible for the plan document.
The generic SDD Skill remains responsible for execution. The
`brainstorm-to-delivery` coordinator constructs and publishes the manifest from
the plan it owns, so no generic Skill change is required in v1.

## MCP Tool Contracts

### `get_workflow_capabilities`

This read-only tool returns a bounded version map including
`workflow_manifest_v1`, plus the enabled operation names for consistency
checking. It performs no workflow read or write and is safe to call before a
workflow id exists.

Old companions do not expose this tool. The Skill treats that as legacy only
when `publish_workflow_manifest` and `settle_workflow_gate` are also absent.

### `publish_workflow_manifest`

The tool accepts a bounded structured document with:

- `schema_version = 1`;
- `workflow_kind = brainstorm_to_delivery`;
- optional existing `workflow_id` for updates;
- `expected_manifest_revision` for compare-and-swap updates;
- workflow state: `skeleton`, `estimated`, or `approved`;
- workspace-relative display paths and cryptographic document digests;
- stable phase, node, edge, and gate ids;
- node kind: milestone, work unit, gate, or placeholder;
- Task index/title and dependency ids where applicable;
- work-unit role, agent type, immutable profile id, and raw work-unit key for
  delegated nodes;
- required reviewer-node ids and resolution mode for gates.

The response includes:

- `workflow_id`;
- accepted `manifest_revision`;
- resulting `graph_revision`;
- normalized manifest state;
- idempotent replay indication.

The server validates the complete document before writing anything. Validation
includes:

- parent-conversation ownership and workspace binding;
- supported schema and workflow kind;
- bounded strings, nodes, edges, gates, and Task count;
- unique stable ids and work-unit keys;
- valid dependency references and an acyclic dependency graph;
- canonical phase/role/agent combinations for this workflow kind;
- profile/agent consistency;
- work-unit-key length and exact manifest-to-role identity, recomputed from
  normalized manifest fields per Contract Amendment A1 (workspace-relative
  paths + agent_type + profile) rather than trusted from the submitted raw key;
- immutable identity for every admitted or terminal node;
- pending-only replacement during a plan revision;
- document digest and manifest-revision CAS.

The same normalized document digest under the same expected state is
idempotent. A stale expected revision returns a typed conflict and the current
revision; the Skill reloads durable state before deciding whether to retry.

### `settle_workflow_gate`

The tool accepts:

- `workflow_id`;
- `manifest_revision`;
- stable `gate_id`;
- expected `graph_revision`;
- gate cycle;
- outcome: `approved`, `changes_requested`, or `blocked`;
- bounded Critical/Important/Minor counts and a bounded adjudication summary.

The backend mechanically verifies that every required reviewer work unit for
that gate/cycle has a terminal run with a validated card summary and that the
caller owns the parent workflow. For a zero-reviewer Design gate it instead
verifies the canonical parent-acknowledgement gate shape. Plan gates cannot have
an empty reviewer set.

The submitted counts and summary are the parent's durable adjudication, not a
server-side vote. The backend does not decide whether a review finding is
factually valid. It rejects approval when the submitted adjudication retains a
Critical or Important issue, when a required review is absent or active, or
when a failed review has no legal recovery. A parent may reject an individual
reviewer finding as invalid, but must record that evidence-based disposition in
the bounded adjudication summary.

Gate settlement is idempotent for the same cycle and payload. A changed outcome
requires a new legal review cycle, never mutation of a prior settlement. Task
and Final execution gates are projected automatically and cannot be passed with
this tool.

### Read APIs

`get_workflow_graph_snapshot(parent_conversation_id)` returns the current safe
projection for live refresh. Historical first render receives the same DTO in
`DbConversationDetail.workflow_graph`.

No public API returns raw manifests, raw work-unit keys, absolute paths, prompts,
or internal routing snapshots.

## Persistence Model

The logical persistence model contains four durable areas. Exact SeaORM entity
split may follow repository conventions, but the invariants and indexed lookup
paths are required.

### Workflow header

One `delegation_workflows` row records:

- workflow id;
- parent conversation id;
- workflow kind and schema version;
- active manifest revision;
- monotonic graph revision;
- workflow state;
- capability version;
- created/updated timestamps.

The parent conversation owns the workflow. Parent deletion cascades through
all workflow records.

### Immutable manifest revisions

`delegation_workflow_manifest_revisions` stores every accepted normalized
manifest document, digest, state, revision, and timestamp. Revisions are
append-only. The header points to the active revision.

The active document is read as a unit, so an immutable validated JSON document
is appropriate. The server uses typed serde structures rather than string
manipulation.

### Node bindings

`delegation_workflow_node_bindings` indexes stable node id and internal
work-unit key to workflow identity. It records introduction/retirement revision
and whether the node has become observed. This enables efficient run-to-node
association and prevents a plan update from discarding admitted work.

Raw work-unit keys and local paths remain backend-only.

### Gate settlements

`delegation_workflow_gate_settlements` stores immutable document-gate cycle,
manifest revision, outcome, bounded counts/summary, and timestamps. A unique key
prevents duplicate settlement of the same gate cycle. Task and Final execution
gates remain derived projection state and do not create rows here.

Manifest publication, active-revision update, node-binding update, and graph-
revision increment commit in one transaction. Gate settlement and graph-
revision increment also commit together. A durable run transition that maps to
a workflow node increments graph revision in the same transaction as the run
write.

## Backend Projection

Add one shared projection function used by desktop commands, Axum handlers, and
conversation-detail loading:

```text
project_workflow_graph_core(connection, parent_conversation_id)
  -> Option<WorkflowGraphSnapshot>
```

It performs one SQLite read transaction:

1. Load the active workflow header and manifest revision.
2. Load active and retained-observed node bindings.
3. Load every durable run for the parent whose internal key maps to those
   bindings.
4. Load gate settlements for the workflow.
5. Group runs by work-unit node and lineage order.
6. Overlay durable run/gate truth on manifest-estimated state.
7. Calculate phase aggregates and the deterministic current node.
8. Produce a redacted frontend DTO.

### Work-unit aggregation

For one work unit, the projection exposes:

- latest durable lifecycle and review/implementation summary;
- total run count;
- latest generation;
- replacement count and latest replacement reason category;
- latest child conversation id and child turn anchor;
- safe session-history identities required by node detail;
- agent/profile display identity;
- runtime and concern counts already allowed by card DTOs.

The raw lineage root and work-unit key remain internal.

### Node-state precedence

Highest precedence wins:

1. Durable blocked/failed state with no legal active recovery.
2. Required terminal run without a validated card summary, projected as blocked
   with a typed missing-summary reason.
3. Reserving or running latest run.
4. Explicit document-gate settlement for the current cycle.
5. Terminal run plus validated card-summary verdict.
6. Retained observed state from an older manifest.
7. Active manifest estimated state.

The projection never lets an older generation overwrite a newer generation or
an estimated node overwrite observed truth.

### Current-node selection

The current selection is deterministic:

1. A blocked gate/work unit that prevents all forward progress.
2. The earliest dependency-ordered reserving/running work unit.
3. A review gate waiting for parent adjudication.
4. The earliest unstarted node whose dependencies are satisfied.
5. Final completion when every required gate and node passes.

Parallel active reviewers share the current phase; the compact view shows their
aggregate instead of choosing one reviewer as globally primary.

### Plan revision behavior

A new manifest revision:

- keeps stable ids and bindings for unchanged nodes;
- may add, reorder, relabel, or remove only unstarted estimated nodes;
- retains started/completed nodes even when the new plan no longer includes
  them, marking removed observed nodes as superseded history;
- atomically replaces the visible pending chain;
- never resets run counts, replacement history, gate cycles, or completed
  status.

## Frontend DTO

The safe DTO is conceptually:

```text
WorkflowGraphSnapshot {
  schema_version
  workflow_id?
  workflow_kind
  manifest_revision?
  graph_revision?
  manifest_state?
  compatibility: manifest | observed_only
  overall_state
  current_phase_id?
  current_node_ids[]
  phases[]
  nodes[]
  edges[]
  gates[]
}
```

Manifest snapshots carry all three optional workflow/revision fields.
Observed-only snapshots leave them absent because no durable workflow header or
manifest exists; the frontend must not invent a workflow id or comparable graph
revision.

Work-unit nodes include safe ids, phase, Task index/title, role, display agent/
profile, projected status, run/replacement counts, latest child session action,
and bounded summaries. Gates contain required/returned/running/blocked counts
and latest settlement outcome.

`DbConversationDetail.workflow_graph` is optional for wire compatibility with
older servers and conversations.

## Historical Loading

Historical loading extends the existing `get_conversation_detail_core` path.
The workflow projection is independent of transcript parsing and is attached to
the returned detail before the frontend renders the overlay.

This avoids a list-to-Graph flash and gives desktop/server clients identical
cold state. Existing per-run metadata injection remains for message-stream
cards; it is not reused as the Graph source of truth.

Compatibility is fail-closed:

1. Active manifest present: return the full estimated structure with durable
   runs and gates overlaid.
2. No manifest, but recognized workflow work-unit keys exist: synthesize an
   observed-only Graph containing only actual work units and derived phase
   groups. Do not add future Tasks or completion percentage.
3. No recognized structured keys: omit `workflow_graph`; show Sessions.
4. Corrupt/unsupported manifest: log a structured backend warning, omit the
   Graph, and keep Sessions. Do not partially parse or guess missing structure.

An observed-only projection never changes workflow budget, lineage, or Skill
state and does not create a manifest as a side effect of reading history.

## Live Updates

Manifest publication, document-gate settlement, and mapped run transitions emit
a backend event containing only parent conversation id, workflow id, and the
new monotonic graph revision.

The frontend workflow store:

1. installs the `DbConversationDetail.workflow_graph` snapshot on cold load;
2. listens for a higher graph revision;
3. refetches `get_workflow_graph_snapshot`;
4. discards responses older than its installed workflow/revision pair;
5. keeps the last valid snapshot while a refresh is in flight.

The frontend does not duplicate backend gate or current-node reducers. This
keeps live and historical projection identical.

Observed-only compatibility has no durable graph clock. A recognized legacy
run admission or terminal transition therefore emits a compatibility-change
nudge containing only the parent conversation id. The frontend refetches the
backend projection and uses a local request generation to discard older
in-flight responses. Reconnect and ordinary conversation-detail reload perform
the same refetch, so a missed nudge cannot corrupt durable history. High-
frequency runtime-stat events continue through the existing Sessions path and
do not trigger whole-Graph refetches.

## Error Handling

Typed publication failures include:

- unsupported schema or workflow kind;
- capability/tool mismatch;
- invalid manifest shape;
- cyclic or missing dependency;
- duplicate node/work-unit identity;
- role/profile/agent mismatch;
- stale manifest or graph revision;
- admitted-node mutation;
- gate not ready;
- gate cycle conflict;
- parent/workspace ownership violation;
- persistence failure.

When capability v1 is active, these failures pause the Skill at the relevant
gate and report the typed condition. The Skill may reload and retry only a stale
revision or an idempotent interrupted write. It does not rewrite a manifest to
work around validation.

Publication and settlement are transactionally all-or-nothing. No partial
manifest, binding, gate, or graph revision is visible.

Frontend projection/load failure keeps Sessions usable. It may show a concise
error state in the Workflow segment when a previously installed workflow is
known, but it must not replace the session list or display stale state as
current.

## Component Boundaries

Expected implementation areas are:

- database migration/entities for workflow headers, manifest revisions, node
  bindings, and gate settlements;
- a backend workflow manifest validator/store/projector under the delegation
  ownership boundary;
- desktop commands plus Axum handlers/router for **snapshot read only**;
  publish/settle/`get_workflow_state` stay on root companion MCP (A4/A5);
- Codeg MCP schemas/listener routing for the two mutation tools and capability;
- run-store hooks that bump graph revision for mapped run transitions;
- `DbConversationDetail` and TypeScript mirror additions;
- a frontend workflow snapshot store and invalidation listener;
- compact stage rail, expanded deterministic Graph, gate group, and node-detail
  components;
- integration in `SubAgentOverlay` and its `MessageListView` callers;
- all ten locale message files;
- `.agents/skills/brainstorm-to-delivery/SKILL.md` manifest/gate/ledger rules.

The Graph layout should use focused React/CSS/SVG components. A general graph
library is not required for the fixed phase/task topology.

## Test Strategy

### Manifest validation and persistence

- Create a skeleton manifest and publish the full estimated plan revision.
- Replay the same digest/payload and verify idempotent success without a new
  revision.
- Race two updates with one expected revision and verify exactly one CAS winner.
- Reject duplicate ids/keys, missing edges, cycles, unsupported roles, profile
  mismatch, oversized documents, and cross-parent ownership.
- Reject removal or identity mutation of reserving/running/terminal nodes.
- Accept atomic replacement of unstarted estimated nodes and retain observed
  superseded nodes.
- Roll back every table and graph revision on injected persistence failure.

### Gate behavior

- Fan out two or more Design/Plan reviewer nodes and keep the gate pending until
  all required runs are terminal.
- Reject majority approval while one required reviewer is active or absent.
- Record parent-approved, changes-requested, and blocked settlements.
- Verify same-cycle same-payload idempotency and reject conflicting mutation.
- Continue original reviewer threads after document revision and advance the
  gate cycle without creating replacement work units.
- Keep Task/final reviewer routing Codex-only.

### Projection and history

- Overlay estimated manifest state with initial, continued, failed, canceled,
  and replacement runs.
- Verify one work-unit node contains all generations and exposes the latest
  valid child session.
- Verify durable run state always wins over estimated state.
- Close/reopen the application and render the identical graph/current phase.
- Load a historical conversation with manifest plus runs and verify no
  list-to-Graph flash.
- Load legacy recognized keys without manifest and verify observed-only nodes,
  no future Tasks, and no write-on-read.
- Admit and complete another recognized legacy run and verify the observed-only
  Graph refetches without inventing a workflow revision or accepting an older
  in-flight response.
- Load unrecognized/native-only and corrupt-manifest conversations and verify
  Sessions fallback.
- Verify snapshot redaction of raw key, absolute paths, prompts, and route data.

### Live ordering

- Publish manifest v2 while a v1 snapshot request is in flight and reject the
  stale response.
- Deliver graph-change events out of order and keep the highest graph revision.
- Transition estimated -> running -> completed and verify the node does not
  move.
- Update concurrent reviewer branches independently and verify compact gate
  counts.
- Settle a gate and verify the next dependency-ready phase lights without a
  full conversation reload.

### Frontend interaction and accessibility

- Default to Workflow only for recognized snapshots and preserve Sessions for
  every conversation.
- Switch segments without losing overlay size/collapse state.
- Expand/collapse without changing current phase or selected node.
- Open observed nodes in the existing main child-conversation tab path; keep
  estimated nodes non-actionable.
- Render long titles, long localized status text, multiple reviewer profiles,
  replacement count, and at least 50 planned Tasks without overlap.
- Verify deterministic keyboard order, accessible node/gate names, focus after
  expansion, and non-color status cues.
- Verify desktop and mobile widths and all ten locale bundles.

### End-to-end scenarios

- Capability absent -> legacy Skill path -> Sessions/observed-only UI.
- Capability/tool disagreement -> hard block before any manifest or reviewer
  dispatch.
- Capability present -> skeleton publish -> concurrent Design gate -> plan
  publish -> full estimated chain -> concurrent Plan gate -> serial SDD Tasks ->
  new final Codex review -> completed Graph.
- Manifest publish failure under active capability hard-blocks before reviewer
  dispatch.
- Plan revision replaces only pending Tasks and survives restart.
- Continue and legal replacement update the original work-unit node and survive
  restart.
- Compaction recovery reloads workflow/ledger state without duplicate workflow,
  manifest, gate, or delegation creation.

### Verification commands

Run focused tests during implementation, then the complete affected frontend,
desktop, server, and MCP matrices:

```powershell
pnpm eslint .
pnpm test
pnpm build

Set-Location src-tauri
cargo fmt --check
cargo check
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings
cargo check --no-default-features --bin codeg-server
cargo test --no-default-features --bin codeg-server --lib
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

## Acceptance Criteria

- A recognized workflow shows a compact current phase and an expandable Graph
  without removing the existing session list.
- The complete estimated Task implementation/review/final chain is visible as
  soon as the implementation plan manifest is published.
- Planned nodes retain stable positions while durable runs light their state.
- Continue and replacement runs never create duplicate work-unit nodes.
- Concurrent reviewers fan out, report independently, and converge on an
  explicit parent-adjudication gate.
- No gate advances by majority vote or before all required reviewers return.
- A skipped independent Design review is durably settled through the explicit
  zero-reviewer parent-acknowledgement gate.
- Closing and reopening a conversation restores the same planned/observed
  structure, gate state, run counts, replacements, and current phase.
- Legacy and corrupt data fail safely to observed-only or Sessions without
  prose inference.
- Capability absence preserves legacy behavior; active-capability publication
  failure hard-blocks with a typed error.
- Desktop and server output the same redacted snapshot and live revision
  behavior.
- All required frontend and Rust checks pass.

## Rejected Alternatives

### Replace the session list with a Graph

The list remains better for dense operational details and native/unstructured
delegations. A hybrid preserves both overview and direct session access.

### Use one visible node per run

Continue and replacement cycles would rapidly overwhelm the diagram and hide
the stable Task/role structure. Runs belong inside a work-unit node.

### Infer stages from task text, plan Markdown, or transcript order

Compaction, parser differences, missing tool output, localization, and prompt
variation make this non-authoritative. It also cannot reliably predict future
Tasks.

### Use only observed work-unit keys

This can group historical work but cannot show the complete estimated chain
when the plan is generated. It remains only a legacy compatibility mode.

### Use a force-directed or draggable graph library

The workflow has deterministic phases, Task order, and gates. A free layout
adds motion, pan/zoom burden, unstable historical geometry, and a dependency
without improving comprehension.

### Treat concurrent review as voting

The Skill explicitly requires evidence-based deduplication and adjudication
with no reviewer priority. Majority voting could advance with an unresolved
valid Critical finding.

### Make manifest publication best-effort

Silent failure would break the guarantee of a complete predicted chain and
reliable historical restoration. Capability negotiation provides legacy
compatibility; once v1 is advertised, publication is control-plane state and
must be a hard gate.

### Modify generic planning and SDD Skills

The workflow coordinator already owns the plan path, review group, Task order,
and Codeg routing keys. Keeping the new contract in
`brainstorm-to-delivery` avoids coupling generic Skills to a Codeg UI feature.

## Contract Amendments (Normative — supersedes conflicts above)

Parent adjudication after parallel Design review (CodeBuddy:KimiK3,
CodeBuddy:Opus4.8, Grok, Codex). Every item below is **required** for v1.
Where this section conflicts with earlier prose, **this section wins**.

### A1. Canonical `work_unit_key` derivation (Critical)

**Problem fixed:** Server “recompute from workspace/branch only” cannot match
Skill keys that embed absolute document paths; Windows paths also breach the
200-character MCP limit.

**Canonical materials (v1, mandatory for Skill + validator):**

| Work unit | Key materials (`\|` separated, no unescaped `\|` in fields) |
| --- | --- |
| Design | `design\|{rel_doc_path}\|reviewer\|{agent_type}\|{profile_id\|none}` |
| Plan | `plan\|{rel_plan_path}\|reviewer\|{agent_type}\|{profile_id\|none}` |
| Task implementer | `task\|{task_index}\|implementer\|{agent_type}\|{profile_id\|none}` |
| Task reviewer | `task\|{task_index}\|reviewer\|{agent_type}\|{profile_id\|none}` |
| Final reviewer | `final_review\|reviewer\|{agent_type}\|{profile_id\|none}` |
| Final fixer (when needed) | `final_review\|fixer\|{agent_type}\|{profile_id\|none}` |

Rules:

1. Paths are **workspace-relative**, forward-slash normalized, lowercased on
   Windows for comparison only after UTF-8 NFC; stored keys use the normalized
   relative form (never absolute paths).
2. `agent_type` is the Codeg enum string (`grok`, `codex`, `code_buddy`, …).
3. Absent profile uses the literal `none`.
4. `task_index` is a positive decimal integer with no leading zeros.
5. Server validation **does not trust** the submitted raw key alone: it
   recomputes the expected key from normalized manifest fields (role, agent,
   profile, task index, relative document path, workflow kind) and requires
   **byte equality** with the submitted key and with every binding.
6. Document digests remain separate manifest fields for drift detection; they
   are **not** embedded in the key.
7. Keys longer than 200 characters after normalization are rejected at publish
   and at delegation admission.
8. Observed-only recognition uses this grammar only (conservative; unknown
   prefixes ignored). Negative cases: ad-hoc keys such as `unit-preboot`.

Skill text (`brainstorm-to-delivery`) must replace absolute-path key materials
with this table during implementation; generic SDD/writing-plans Skills remain
unchanged.

### A2. Gate-cycle legality and review freshness (Critical)

Document gates have a 1-based `gate_cycle` per `gate_id`.

1. Cycle 1 begins when the gate’s required reviewer set is first published.
2. A non-`approved` settlement (`changes_requested` or `blocked`) ends the
   current cycle and opens cycle N+1 only after a new legal review set exists.
3. Settlement of cycle N verifies that **each** required reviewer work unit has
   at least one run that is:
   - admitted **after** the previous cycle’s settlement timestamp (for N=1:
     admitted after the gate’s introduction revision / prior document digest
     binding for that cycle);
   - terminal with a **validated** card summary;
   - associated with this workflow/node and the **current** reviewed artifact
     digest for cycle N.
4. Stale terminal runs from cycle N−1 **cannot** satisfy cycle N.
5. Same-cycle same-payload settlement is idempotent; conflicting mutation of a
   settled cycle is rejected (`gate_cycle_conflict`).

Implementation may store the association as an immutable
`delegation_workflow_run_bindings` (or equivalent) row at admission:
`task_id`, `workflow_id`, `node_id`, `gate_id?`, `gate_cycle?`,
`manifest_revision`, `artifact_digest?`. Settlement validates the expected set
from these rows, not by scanning all historical runs for the key alone.

### A3. Workflow uniqueness and create idempotency (Critical)

1. At most **one** active workflow per
   `(parent_conversation_id, workflow_kind)` (unique index).
2. Initial create without `workflow_id` is idempotent when the client supplies a
   bounded `publication_token` (UUID) stored on the header; replay of the same
   token + same normalized digest returns the existing workflow without a new
   header.
3. A second create with a different token while an active header exists returns
   a typed conflict naming the existing `workflow_id`.
4. Projection always loads the single active header for that parent+kind.

### A4. Mutation authorization surface (Critical)

1. `publish_workflow_manifest`, `settle_workflow_gate`, and agent recovery reads
   are **root companion MCP only** (UDS listener + shared `_core`), role
   `CompanionRole::Root`, feature token `workflow_v1`.
2. Desktop Tauri commands and Axum HTTP expose **read-only**
   `get_workflow_graph_snapshot` (and conversation-detail attachment). They
   **must not** expose publish/settle.
3. Delegation-child companions do not list mutation tools.

### A5. Agent recovery read tool (Critical)

Add root-only MCP tool `get_workflow_state`:

- Input: optional `workflow_id` or resolve by parent conversation + kind.
- Output (agent-facing, still no frontend leakage of secrets beyond what the
  parent already used): `workflow_id`, capability mode, manifest state,
  `manifest_revision`, `graph_revision`, document relative paths + digests,
  gate ids/cycles/latest settlement outcomes, node ids with role/agent/profile,
  work-unit keys, and dependency readiness summary.
- Included in `workflow_manifest_v1` consistency checks with publish/settle.
- Frontend continues to use the **redacted** `WorkflowGraphSnapshot` only.

### A6. Final fix / re-review graph (Critical)

Reconcile expanded Graph + SDD without modifying generic SDD skill contracts
beyond what `brainstorm-to-delivery` already coordinates:

1. Final phase contains two stable work units when a fix is required:
   - Final reviewer (`final_review|reviewer|codex|…`)
   - Final fixer (`final_review|fixer|grok|…`) — estimated only until a final
     review requests changes; may remain unused if Final approves first pass.
2. Final reviewer continue for **scoped re-review after a final fix** is an
   allowed graph/Skill path (same work-unit node, new run, new gate cycle /
   execution-gate evaluation). Unexpected-interruption continue remains
   allowed as today.
3. Task-level fix/re-review stays implementer + reviewer work units already in
   the manifest (continue, not new nodes).
4. Execution-gate auto-pass never invents a second Final reviewer identity.

### A7. Execution-gate truth table (Critical)

Task and Final execution gates are projected only (no `settle_workflow_gate`).

**Implementer terminal pass:** validated card summary with implementation
status `done` or `done_with_concerns`.

**Implementer block / not ready:** `blocked`, `needs_context`, failed/canceled
terminal without legal recovery, or missing/invalid summary.

**Reviewer terminal pass:** validated summary verdict `approve` or
`approve_with_minors`.

**Reviewer not pass:** `request_changes`, `block`, missing/invalid summary, or
summary that does not cover the implementer’s latest terminal commit set /
artifact digest for that work unit.

**Task gate passes** only when implementer has a terminal pass **and** the
paired reviewer has a later-or-equal terminal pass that references that
implementer generation/artifact.

**Final gate passes** when the Final reviewer has a terminal pass covering the
branch tip required by SDD; if Final requests changes, Final fixer must reach
implementer pass and Final reviewer must produce a new pass (A2 freshness).

### A8. Post-approval plan revision state machine (Important)

1. Material plan change after `approved` demotes manifest state to `estimated`
   (or `revision_pending` if a distinct enum is preferred; v1 may reuse
   `estimated` with a non-null `supersedes_approved_revision`).
2. Publish a new estimated revision (CAS); unstarted nodes may be replaced;
   observed nodes retained as superseded history.
3. Plan document gate opens a new cycle; Task admissions for **new** work are
   blocked until the Plan gate is re-approved.
4. Already-running Task work units continue under retained bindings; they are
   not deleted.
5. After Plan re-approval, state returns to `approved` and new Task admissions
   may proceed against the active revision.

### A9. Run → node mapping (Important)

1. Runs whose `work_unit_key` matches an active or retained binding attach to
   that node.
2. Runs with a recognized-shape key that matches no binding attach to a
   typed retained/orphan observed bucket (never silently dropped from
   Sessions; Graph shows them only as retained observed if recognized).
3. `NULL`/unrecognized keys under an active manifest are ignored by the Graph
   and remain in Sessions.
4. Projection never fails conversation detail load because of orphan runs.

### A10. Run-store graph-revision hook (Important)

On durable lifecycle transitions that change projected node state
(reserve/admit, promote, terminal settle, legal replacement link, provisional
abandon/reconcile that changes latest generation):

1. Look up binding by `(parent_conversation_id, work_unit_key)`.
2. If absent: **no** workflow table write (cheap no-op for non-workflow
   conversations).
3. If present: bump `delegation_workflows.graph_revision` in the **same**
   transaction as the run write; lock order: run row → binding → workflow
   header.
4. After commit, emit `workflow_graph://changed` via shared `EventEmitter`.
5. High-frequency runtime-stat patches do **not** bump graph revision and do
   not refetch the Graph; Graph DTO may omit volatile per-second stats and
   reuse Sessions live store for those fields, or show last terminal-time
   stats only.

### A11. Observed-only recognition (Important)

Recognizer accepts only A1 prefixes/arities. Observed-only Graph never invents
future Tasks, workflow ids, or graph revisions. Mid-flight conversations at
feature ship (keys present, no manifest) stay observed-only until a future
explicit publish (no write-on-read).

### A12. Zero-reviewer Design gate shape (Important)

Canonical self-review Design gate requires:

- `resolution_mode = self_review`;
- empty `required_reviewer_node_ids`;
- Design document relative path + digest present;
- Plan gates **cannot** use this shape.

Skeleton may start with a provisional Design gate and revise after the Skill’s
conditional Design-review decision.

### A13. Overlay empty-state (Important)

If `workflow_graph` is present, `SubAgentOverlay` **must mount** even when
session/activity count is zero, so skeleton/estimated graphs are visible before
the first delegation. Sessions segment may be empty. Conversations without
`workflow_graph` keep today’s null-when-empty behavior and never show a
disabled Workflow segment (control omitted or Sessions-only).

### A14. Admission enforcement owner (Important)

When capability v1 is active and the parent conversation has an active
workflow header:

- Broker/`delegate_to_agent` and `continue_delegation` admission **must**
  reject keys that are not active bindings with matching role/agent/profile
  (typed error), except during pre-approval Design/Plan stages where only
  published Design/Plan nodes are valid.
- Dependency readiness for Task nodes is enforced at admission once the
  manifest is `approved` (prior Task execution gate must pass).
- Legacy conversations with no workflow header: admission unchanged (no-op).

### A15. Capability transport and bounds (Important)

1. Parent injects `workflow_v1` into companion `--features` only when mutation
   tools and persistence paths are enabled; `get_workflow_capabilities`
   answers **locally** from `CompanionFeatures` so catalog/response agreement
   is structural.
2. Concrete v1 bounds (validator + UI agree): Tasks ≤ 100; nodes ≤ 400;
   edges ≤ 800; gates ≤ 50; adjudication summary ≤ 4 KiB; card summary fields
   per existing card limits; total normalized manifest JSON ≤ 512 KiB.
3. Live events:
   - `workflow_graph://changed` →
     `{ parent_conversation_id, workflow_id, graph_revision }`
   - `workflow_graph://compatibility_nudge` →
     `{ parent_conversation_id }`
   Both use `EventEmitter` (Tauri + WebSocket). Frontend discards stale
   snapshot responses using `graph_revision` for manifest mode; observed-only
   uses local request generation.

### A16. Card summary Skill obligation (Important)

`brainstorm-to-delivery` prompt templates for Design/Plan reviewers,
implementers, Task reviewers, Final fixer, and Final reviewer must require a
validated terminal card summary. Missing summary blocks document settlement
and execution-gate advance (A2/A7).

### A17. Display-string safety (Important)

Frontend DTO strings (titles, summaries, labels) are either:

- server-generated opaque public ids / enums, or
- bounded agent text run through a redaction rejector that fails closed on
  absolute paths, `work_unit_key`-shaped tokens, and prompt-like fences.

Tests must include malicious strings in every returned free-text field.

### A18. Naming and read API notes (Minor, still normative)

- Historical attach point: `get_folder_conversation_core`.
- Snapshot clock for manifest mode: `graph_revision`.
- `DbConversationDetail.workflow_graph` is `Option` with
  `skip_serializing_if = "Option::is_none"`; TS mirror optional.

## Residual Risks

- An agent can violate a prose Skill contract. Capability-backed validation,
  admission enforcement (A14), and hard gates detect inconsistent
  manifests/keys, but cannot force a model to call a tool it never attempts.
  The parent must treat missing publication as unfinished orchestration.
- Very large plans can create visually dense expanded graphs. Bounded manifests
  (A15), deterministic Task rows, and progressive mounting contain this without
  hiding the complete chain.
- Old conversations without structured keys cannot be reconstructed. Sessions
  remains the honest fallback.
- Parent gate adjudication is an explicit side effect. An interruption between
  reasoning and settlement leaves the gate pending; recovery must reread runs
  via `get_workflow_state` (A5), recreate any missing durable report, and settle
  only from current evidence.
- Manifest revisions can disagree with a manually edited plan file after
  publication. The stored plan digest exposes drift; execution must hard-block
  until the Skill publishes a matching reviewed revision.
- Workspace folder moves after workflow creation: v1 freezes workspace-relative
  identity at header creation; moves may force observed-only or republish — not
  silently rewritten keys.
- Pre-A1 Skill keys (absolute paths, missing `agent_type` field) are **not**
  recognized by A11; those conversations use Sessions, not observed-only.
  Observed-only applies only to A1-grammar keys without a manifest.

## Contract Amendments Round 2 (Normative — after re-review)

Parent adjudication of re-review (KimiK3 Important N-1; Grok R1–R2; Codex
Important residuals). Supersedes conflicting A1–A18 wording.

### B1. Path normalization stored form (closes A1 case conflict)

Stored and submitted keys use **one** serialized form:

1. UTF-8 NFC
2. path separators → `/`
3. on Windows, path field lowercased **before** key construction
4. reject any field containing unescaped `|` (including Unix paths)

Byte equality is evaluated on this form only. Skill, publish validator, and
admission all construct keys the same way.

### B2. Admission: active vs retained-observed (closes A14 vs A8.4)

- **First dispatch** (generation-1, no lineage): requires an **active** binding.
- **Continue / legal replacement** on existing lineage: also satisfied by a
  **retained-observed** binding with matching role/agent/profile (A8.4
  already-running Task units after plan revision).
- Never admit against a fully retired node with no retained-observed flag.

### B3. Execution-gate artifact coverage is mechanical (closes A7 free-text)

Do **not** parse free-text SHAs from card summaries.

For Task/Final:

1. At implementer terminal, run binding records `artifact_digest` as the
   workspace HEAD commit id (or empty + generation-only when unavailable).
2. Reviewer admission records `reviewed_task_id`, `reviewed_implementer_generation`,
   and expected `artifact_digest` copied from that implementer binding.
3. Reviewer pass requires: validated summary verdict in A7 pass set **and**
   `reviewed_implementer_generation` ≥ latest implementer terminal generation
   for that Task **and** matching `artifact_digest` when present.
4. `CardSummary::Review` schema need not grow; authority is the run-binding
   row, not summary prose.

### B4. `get_workflow_state` recovery payload (closes A5 gap)

In addition to A5 metadata, return a bounded per-node/gate evidence block:

- latest run `task_id`, status, generation, replacement linkage;
- whether card summary validated;
- gate_cycle association and artifact digests from run bindings;
- enough for parent adjudication without a perfect local ledger.

Hard size bound: same class as A15 (truncate oldest completed nodes first if
needed, never drop active gate required set).

### B5. Transaction ordering (closes A10 impossible lock order)

- **New admission:** validate binding → insert run + insert run_binding → bump
  graph_revision (single SQLite transaction). No pre-existing run row.
- **Existing-run transition:** update run → update run_binding if needed → bump
  graph_revision.
- Do not prescribe OS-level row locks; rely on SQLite transaction atomicity and
  CAS on `graph_revision` / `manifest_revision` where concurrent publishers
  exist.

### B6. Final-phase admission readiness (closes Final early start)

When capability v1 + active approved (or post-plan) manifest:

- Final **reviewer** first dispatch only when every **active** Task execution
  gate has passed.
- Final **fixer** only after Final reviewer terminal is non-pass
  (`request_changes` / `block`) for the current Final cycle.
- Final **re-review** continue only after Final fixer terminal pass for that
  cycle (B3).

### B7. Pre-A1 keys and compatibility claim (closes A11 overclaim)

A11 stands: only A1 grammar is recognized. Pre-A1 keys → Sessions, not
observed-only. Document this in residual risks (done). Optional future
`legacy_v0` recognizer is **out of v1**.

### B8. Publication-token mismatch (closes A3 gap)

Same `publication_token` with a **different** normalized digest → typed
idempotency mismatch, no mutation.

### B9. Capability tool set (closes “two tools” drift)

`workflow_manifest_v1` requires all four root tools present and consistent:
`get_workflow_capabilities`, `get_workflow_state`, `publish_workflow_manifest`,
`settle_workflow_gate`. Any other combination is inconsistent/hard-block or
legacy (none of the four).

### B10. Required test additions

Add explicit tests for: B3 stale artifact coverage rejection; A3/B8
publication-token races and mismatches; A4 root vs child vs HTTP mutation
authorization; B6 Final fix/re-review admission; A10/B5 provisional abandon
clock; pre-A1 keys Sessions-only; A14/B2 continue on retained-observed after
plan revision; A2 cycle N+1 rejects cycle-N runs.

### B11. Optional reviewer compact counts

Compact `returned/required` counts include **required** reviewers only.
Optional reviewers appear in expanded Graph but not in the required
denominator.

### B12. Replacement field vocabulary

DTO/node detail exposes separately: `run_count` (all generations),
`active_child_generation`, `replacement_count`, document `gate_cycle` (when
applicable), and `round_count` = continue rounds on the active child. Do not
use max(generation) alone as lineage position across replacements.

### B13. Reviewer pass targets exact implementer run (closes B3 replacement hole)

B3 generation comparison is **informational only**. A Task/Final reviewer
terminal pass is valid only when:

1. `reviewed_task_id` equals the **exact** latest terminal implementer
   (or Final fixer) `task_id` for that work unit under current lineage
   (`lineage_root` + highest `lineage_ordinal` / admission clock), **or**
2. when using digests: `artifact_digest` matches that same exact run’s digest
   **and** `reviewed_task_id` still equals that run.

A generation-5 review of a pre-replacement child **cannot** pass a
generation-1 replacement implementer even if digests collide or are empty.
B10 must include this replacement-stale-approval regression.

### B14. Task-pair freeze on first admission (closes stranded reviewer)

When **either** node of a Task implementer/reviewer pair becomes observed
(first admitted run):

1. Both implementer and reviewer bindings for that Task index are **frozen**
   against plan-revision retirement (they remain active or retained-observed
   until that Task execution gate completes).
2. A plan revision **must not** drop the still-unobserved partner while its
   pair-mate is observed/incomplete.
3. If the plan no longer wants that Task, the Skill must either complete the
   gate under the frozen pair **or** publish a new manifest revision that
   sets overall `workflow_state = blocked` (or marks the frozen Task pair
   `node_outcome = canceled` while retaining both bindings). Silent drop of
   the unobserved partner is forbidden. Conversation stop is **not** a
   durable cancel; recovery must still see frozen bindings until an explicit
   publish records cancel/block.

B10 must include: implementer started, reviewer not yet dispatched, plan
revision attempted → partner reviewer binding retained and first reviewer
dispatch still legal.
