# Brainstorm-to-Delivery Workflow Graph Design

Date: 2026-07-26

Status: Approved in conversation on 2026-07-26; awaiting written-spec review.

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
- `get_conversation_detail_core` already recovers historical child bindings and
  all durable run snapshots before returning `DbConversationDetail`, then
  injects run metadata into historical tool-call blocks.
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

- capability tool and both mutation tools absent: legacy companion;
- capability tool returns v1 false and both mutation tools are absent: legacy
  mode on a new companion;
- capability tool returns v1 true and both mutation tools are present: v1 mode;
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
- work-unit-key length and exact manifest-to-role identity, recomputed from the
  parent workspace/branch identity and normalized manifest fields rather than
  trusted from the submitted raw key;
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
- desktop commands plus Axum handlers/router for publish, settle, and snapshot;
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

## Residual Risks

- An agent can violate a prose Skill contract. Capability-backed validation and
  hard gates detect inconsistent manifests/keys, but cannot force a model to
  call a tool it never attempts. The parent must treat missing publication as
  unfinished orchestration.
- Very large plans can create visually dense expanded graphs. Bounded manifests,
  deterministic Task rows, and progressive mounting contain this without
  hiding the complete chain.
- Old conversations without structured keys cannot be reconstructed. Sessions
  remains the honest fallback.
- Parent gate adjudication is an explicit side effect. An interruption between
  reasoning and settlement leaves the gate pending; recovery must reread runs,
  recreate any missing durable report, and settle only from current evidence.
- Manifest revisions can disagree with a manually edited plan file after
  publication. The stored plan digest exposes drift; execution must hard-block
  until the Skill publishes a matching reviewed revision.
