# Simple Workflow DAG Overlay Design

**Date:** 2026-08-23

**Status:** Approved for implementation planning after repository-backed design
review and interactive mock review.

**Scope:** Frontend rendering for
`WorkflowGraphSnapshot.compatibility === "simple"`. No backend DTO, projector,
store API, Plan/progress format, or Skill contract changes.

**Related designs:** This adds the Simple-only node-link view that was out of
scope in
`docs/superpowers/specs/2026-07-30-workflow-overlay-ui-design.md`. It preserves
the Simple-only writable workflow decision in
`docs/superpowers/specs/2026-08-11-simple-workflow-v2-retirement-design.md`.

## Repository baseline

The implementation must start from the current repository, not from an
intermediate conversation mock:

- `WorkflowGraphPanel` immediately renders `SimpleWorkflowProjection` for a
  Simple snapshot. That projection is a flat list; it does not use phase lanes
  or render `snapshot.edges`.
- The collapsible phase lanes and `workflow-graph-edges` dependency list are
  currently the **manifest/observed-only** branch. They are not an uncommitted
  Simple implementation.
- `SubAgentOverlay` always mounts the Simple panel in the compact workflow
  segment and hides `workflow-expand-toggle` for Simple.
- The overlay width is 288px by default and is resizable from 224px through
  448px (`src/lib/overlay-size-storage.ts`). There is no 448–768px expanded
  Simple width.
- `project_simple_mode` emits one Tasks phase, no gates or completion card, and
  a routed graph of implementer/reviewer work units when Plan routing exists.
  Legacy Simple projections can still contain one node per Task with no
  implementer role.

This design replaces only the flat `SimpleWorkflowProjection` body. The
manifest/observed-only branch remains behaviorally unchanged.

## Problem

Simple brainstorm-to-delivery is the only writable workflow path, but its
overlay currently renders work units as an ordered list. The user can see
statuses and sessions, but cannot see the dependency topology emitted by the
projector—especially reviewer fan-out and the fan-in that unlocks the next
implementer.

The snapshot already contains the required topology. For the routed reference
workflow below, `project_simple_mode` emits seven nodes and seven edges:

```text
T1 implementer → T1 primary review → T2 implementer
                                      ├→ T2 primary review ─┐
                                      └→ T2 auxiliary review ─┤
                                                              ↓
                                                        T3 implementer
                                                              ↓
                                                       T3 primary review
```

A left-to-right six-rank graph cannot fit the 224–448px overlay. The graph must
therefore use top-to-bottom ranks, with siblings placed side by side.

## Decision

Replace the Simple flat list with a vertically ranked node-link DAG. Render
edges in SVG and nodes as absolutely positioned native HTML buttons over the
SVG. This preserves reliable focus, text layout, tooltips, and screen-reader
semantics without `foreignObject`.

```text
SubAgentOverlay (Simple workflow segment)
  WorkflowGraphPanel (compatibility dispatcher only)
    SimpleWorkflowDagPanel
      Plan / progress links
      backend partial-projection warning
      Tasks section (status + Task current/total)
        WorkflowDagCanvas (valid canvas or invalid-topology fallback)
          horizontal scroll viewport
            relative-size inner canvas
              SVG edge layer (aria-hidden, pointer-events none)
              HTML node-button layer (rank-order DOM)
        selected-node detail when selection exists

    NonSimpleWorkflowGraphPanel
      existing lanes, dependency list, completion/history UI (unchanged)
```

Do not add a graph library. The current Simple graph is small, layered, and
structurally constrained enough for a focused O(V log V + E) layout helper.

## Goals

1. Show the Simple snapshot as a spatial DAG whose fan-out and fan-in are
   visible without opening a dependency list.
2. Treat `snapshot.nodes` and `snapshot.edges` as the only graph authority.
   Never synthesize topology from Task index, role, or `node.deps`.
3. Preserve Simple chrome and behavior: workflow/sessions tabs, overall state,
   exact Plan/progress links, file watching, warnings, node status, operational
   stats, and child-session opening.
4. Keep one panel-local selected node and one detail card.
5. Keep manifest/observed-only rendering and tests unchanged.
6. Handle malformed, empty, and temporarily unmeasured graphs without a throw
   or a misleading partial drawing.
7. Make the dependency topology available to keyboard and screen-reader users,
   not only to sighted pointer users.

## Non-Goals

- Backend projection, DTO, manifest, gate, completion-protocol, or store API
  changes.
- A general-purpose graph viewer or editor, arbitrary edge routing, pan/zoom,
  minimap, drag-to-reorder, or third-party graph library.
- Changing Simple Plan/progress documents or `register_simple_workflow`.
- Persisting selection or scroll position across panel unmounts.
- Virtualizing large graphs in v1.
- Replacing the manifest/observed-only compact phase rail or expanded graph.
- Token or monetary cost display; those values are not in the snapshot.

## Product choices

| Choice | Normative value |
|---|---|
| Placement | Non-collapsible DAG inside the Simple Tasks section |
| Direction | Top to bottom; same-rank siblings share one horizontal row |
| Edge carrier | SVG paths behind the nodes |
| Node carrier | Native HTML buttons, not SVG text or `foreignObject` |
| Dependency list | No visible `workflow-graph-edges` list in Simple |
| Detail | At most one card; exactly one immediately below the canvas/fallback when selection exists |
| Expand toggle | Hidden for Simple; the DAG is the compact workflow body |
| Other phase lanes | Do not render Design, Plan, or Final lanes for Simple |
| Narrow overflow | Horizontal scrolling inside the canvas only |
| Vertical overflow | Existing `sub-agent-overlay-list` owns vertical scrolling |
| Library | None |
| Manifest/observed-only branch | Unchanged |

## Snapshot contract and invariants

The frontend remains defensive even though the production projector already
honors these invariants:

1. `node_id` is unique and non-empty within `snapshot.nodes`.
2. Every edge endpoint names a node in the same snapshot.
3. A `(from, to)` relation occurs at most once. `edge.id` is optional display
   metadata and is not required for identity.
4. The graph is acyclic and has no self-loop.
5. Production routed Simple edges connect adjacent logical ranks. This is true
   for implementer → reviewer and reviewer(s) → next implementer. The v1 layout
   intentionally fails over for a future transitive/long edge instead of
   drawing it through an intermediate node.
6. Simple nodes belong to the Tasks phase. The Tasks lane metadata comes from
   `buildPhaseRail`, but layout always consumes the full snapshot node/edge
   arrays.
7. `current_node_ids` may be empty or stale. Unknown current IDs are ignored;
   they do not invalidate an otherwise usable graph.
8. `node.deps` is never used for layout or accessible dependency text. Using it
   would create a second topology authority beside `snapshot.edges`.

## Component boundaries

### `WorkflowGraphPanel`

Turn the exported component into a compatibility dispatcher that renders one
of two child components. Do not keep non-Simple hooks above the Simple early
return: today that causes the Simple path to initialize manifest lane state and
a second subscription to the shared live clock unnecessarily.

```text
WorkflowGraphPanel
  compatibility === "simple" → SimpleWorkflowDagPanel
  otherwise                  → NonSimpleWorkflowGraphPanel
```

This split also makes a runtime compatibility change unmount one branch and
clear branch-local state cleanly, without conditional-hook ordering problems.
Give `SimpleWorkflowDagPanel` a React key scoped to `conversationId`; only when
that ID is unavailable, fall back to the pair of Simple locator paths. The key
prevents one render of stale selection when two conversations reuse canonical
IDs such as `simple-task-1`.

### `SimpleWorkflowDagPanel`

Owns:

- exact Plan/progress file controls and the existing backend warning banner;
- gate-free Tasks phase metadata;
- a selection scope keyed by `conversationId` (falling back to the two locator
  paths only when no conversation ID is available);
- selected-node state and its reconciliation across live snapshots;
- the selected-node detail and live operational clock; and
- child-session opening through `canOpenWorkflowNode`.

It passes nodes, edges, current IDs, selection, the existing module-level
`nodeDisplayTitle` function, and an `onSelect` callback to
`WorkflowDagCanvas`. Passing the formatter avoids both duplicated fallback
logic and a circular import between the panel and canvas modules.

### `WorkflowDagCanvas`

Owns:

- measuring the real content-box width;
- calling the pure layout helper for LTR or RTL;
- the scroll viewport, SVG edge layer, HTML node-button layer, and accessible
  relationship descriptions; and
- the bounded invalid-graph fallback.

It does not own workflow selection semantics, file controls, live clocks, or
session navigation.

### `workflow-dag-layout.ts`

A pure, deterministic module. It must not read the DOM, mutate its inputs, use
locale APIs, or depend on React.

## Layout helper contract

Input:

```ts
interface WorkflowDagLayoutInput {
  nodes: readonly WorkflowNodeSnapshot[]
  edges: readonly WorkflowEdgeSnapshot[]
  viewportWidth: number // finite and > 0
  direction: "ltr" | "rtl"
}
```

Output:

```ts
interface LaidOutNode {
  nodeIndex: number
  nodeId: string
  rank: number
  x: number
  y: number
  width: number
  height: number
}

interface LaidOutEdge {
  edgeIndex: number
  edgeId: string | null
  from: string
  to: string
  path: string
}

type WorkflowDagLayoutError =
  | "empty"
  | "invalid_width"
  | "invalid_node_id"
  | "duplicate_node"
  | "duplicate_edge"
  | "dangling_edge"
  | "cycle"
  | "unsupported_edge_span"

type WorkflowDagLayoutResult =
  | {
      ok: true
      canvasWidth: number
      height: number
      nodes: LaidOutNode[]
      edges: LaidOutEdge[]
    }
  | { ok: false; error: WorkflowDagLayoutError }
```

`nodeIndex` and `edgeIndex` are source-array indices used to recover the input
records without copying DTOs into layout output. Valid node elements use the
unique `nodeId` as their React key; stateless edge paths use `edgeIndex` because
`edge.id` is optional. No partial coordinates are returned with an error.
Successful `nodes` are ordered by rank and then the in-rank comparator for
direct DOM rendering; successful `edges` retain source edge order. Normalize an
absent edge ID to `null`.

### Validation and graph construction

Perform validation before geometry:

1. Reject an empty node array as `empty`.
2. Reject a non-finite or non-positive width as `invalid_width`.
3. Reject a node ID whose trimmed value is empty as `invalid_node_id`.
4. Build a node-id map and reject duplicate IDs as `duplicate_node`.
5. Reject duplicate `(from, to)` relations as `duplicate_edge`.
6. Reject a missing endpoint as `dangling_edge`.
7. Treat a self-loop as `cycle`.
8. Run Kahn's algorithm. If fewer than all nodes are emitted, return `cycle`.
9. Compute ranks, then require every edge to span exactly one rank. Otherwise
   return `unsupported_edge_span`.

These checks define error precedence when an input has multiple defects: scan
all node IDs for blanks before detecting duplicate nodes, scan all duplicate
relations before endpoint validation, and resolve all endpoints before checking
self-loops. The source edge order is retained for rendering after validation.

### Rank

Compute longest-path rank during deterministic Kahn traversal:

```text
rank(n) = 0                                      when indegree(n) == 0
rank(n) = 1 + max(rank(p) for each predecessor) otherwise
```

Isolated nodes are valid rank-0 nodes. The available zero-indegree set always
pops the smallest source node index so equivalent inputs remain deterministic.

### Sort within a rank

Sort with a stable comparator:

1. `task_index` ascending; `null`/`undefined` last;
2. exact role order `implementer`, `author`, `reviewer`, other, missing; and
3. source node index.

For the Simple routed projector this preserves primary-reviewer then
auxiliary-reviewer order because both reviewers tie on Task index and role and
the projector emits them in policy order.

### Geometry

Use one node width for the entire graph so columns and edge anchors remain
consistent. The original name `NODE_MIN_WIDTH = 148` was contradictory because
the algorithm then shrank below it; use separate ideal and minimum tokens.

| Token | Value |
|---|---:|
| `NODE_IDEAL_WIDTH` | 148 |
| `NODE_MIN_WIDTH` | 96 |
| `NODE_HEIGHT` | 48 |
| `NODE_GAP_X` | 12 |
| `RANK_GAP_Y` | 28 |
| `PAD_X` | 8 |
| `PAD_Y` | 8 |
| `ARROW_END_GAP` | 8 |

Let `maxRankSize` be the maximum node count in any rank:

```text
availablePerNode =
  (viewportWidth - 2*PAD_X - (maxRankSize - 1)*NODE_GAP_X) / maxRankSize

nodeWidth = min(NODE_IDEAL_WIDTH,
                max(NODE_MIN_WIDTH, floor(availablePerNode)))

requiredWidth =
  2*PAD_X + maxRankSize*nodeWidth + (maxRankSize - 1)*NODE_GAP_X

canvasWidth = max(viewportWidth, requiredWidth)
```

For each rank containing `k` nodes:

```text
rowWidth = k*nodeWidth + (k - 1)*NODE_GAP_X
startX = (canvasWidth - rowWidth) / 2
x(i) = startX + i*(nodeWidth + NODE_GAP_X)
y(rank) = PAD_Y + rank*(NODE_HEIGHT + RANK_GAP_Y)
```

```text
height = 2*PAD_Y
       + rankCount*NODE_HEIGHT
       + (rankCount - 1)*RANK_GAP_Y
```

At a measured `viewportWidth` of 288px, a two-reviewer rank uses 130px nodes
and does not scroll. At a measured 224px it uses 98px nodes. These are helper
inputs, not stored overlay widths: the real viewport is narrower after panel
padding. Three or more siblings can force horizontal scroll after the 96px
minimum is reached.

### Direction and edge paths

Calculate LTR boxes first. For RTL, mirror every box before deriving edge
paths:

```text
x_rtl = canvasWidth - x_ltr - nodeWidth
```

For each adjacent-rank edge:

- start at the source bottom-center;
- end `ARROW_END_GAP` pixels above the target top-center;
- use a cubic path whose two control points share the midpoint Y.

The pure helper returns only the path. `WorkflowDagCanvas` applies
`marker-end` using a per-canvas marker ID generated with React `useId` and
sanitized to `[A-Za-z0-9_-]` before use in `url(#...)`.

Derive paths from the final mirrored boxes. Never mirror the rendered SVG with
CSS, because that would also reverse path semantics and any future labels.
The SVG sits behind the HTML nodes and has `pointer-events: none`.

## Width measurement and resizing

The helper receives the **actual canvas viewport content width**, not the
stored overlay width. Padding in the overlay and Tasks section makes those
numbers different.

- Observe the canvas viewport with `ResizeObserver` and use
  `entry.contentRect.width`.
- On mount, synchronously sample `getBoundingClientRect().width` as a fallback;
  retain a window-resize fallback only when `ResizeObserver` is unavailable.
- Keep the measured root mounted for every non-empty snapshot. While its width
  is zero (hidden or not measured), render no inner topology and set
  `aria-busy="true"`; otherwise the component would have no element to observe.
  Do not emit `invalid_width` UI.
- Recompute the pure layout when positive width, direction, nodes, or edges
  change.
- Derive direction from `useLocale()` with the same current rule as
  `I18nProvider` (`ar` is RTL; the other supported locales are LTR). Do not read
  `document.dir` during render, because the provider applies that DOM attribute
  in an effect after a locale change.
- Set `dir` on the measured canvas root from that same value so logical CSS,
  bidirectional text defaults, and horizontal-scroll semantics switch in the
  same render as geometry.
- Preserve user horizontal scroll during ordinary snapshot updates. A resize
  may let the browser clamp an out-of-range `scrollLeft`; do not forcibly reset
  it.
- Use `overflow-x-auto overflow-y-hidden` on the canvas. The existing outer
  overlay list remains the only vertical scroll container.

Overlay tests must provide a deterministic `ResizeObserver` mock; production
code must not hardcode 288px to make jsdom pass.

## Simple UI contract

### Existing chrome

Preserve:

- `data-testid="workflow-graph-panel"` and
  `data-compatibility="simple"`;
- `simple-file-links`, `simple-progress-link`, and exact path opening;
- `simple-projection-warning` when the locator is missing, the snapshot has a
  projection warning, or any node has a projection warning;
- the external Simple Plan/progress file-watch lifecycle in `SubAgentOverlay`;
  and
- the overlay's workflow/sessions tabs and overall-state badge.

The backend partial-projection warning remains distinct from a client graph
validation error. Do not reuse `simplePartialProjection` for a cycle or
dangling edge: its current copy specifically refers to Plan/progress parsing.

### Tasks section

- Render one non-collapsible
  `data-testid="workflow-graph-lane-tasks"` section.
- Derive the Tasks `PhaseRailItem` with
  `buildPhaseRail({ ...snapshot, gates: [] })`. Stripping gates is mandatory so
  malformed data or legacy test fixtures cannot leak manifest gate progress
  into Simple.
- Header content is `WorkflowStatusIcon`, `phase.tasks`,
  `phaseStatus.<status>`, then `phaseProgressFragments` when
  `taskProgress` exists.
- Legacy Simple projections may have no `role === "implementer"`; when
  `taskProgress` is null, retain `simpleTaskCount` using `snapshot.nodes.length`.
- Do not render Design, Plan, or Final lane elements.

### Canvas DOM

| Selector | Contract |
|---|---|
| `workflow-dag-canvas` | Stable measured root for every non-empty graph; valid state is the horizontal scroll viewport and named `role="group"` |
| `workflow-dag-svg` | Valid-state SVG edge layer; absent while unmeasured or invalid |
| `workflow-dag-node-{node_id}` | Native node button for production-unique Simple IDs |
| `workflow-dag-edge-{edgeIndex}` | One SVG path; also carries `data-from`, `data-to`, and optional `data-edge-id` |

Use the source edge index rather than `${from}-${to}` as the path test ID and
React key. Concatenated endpoints are not collision-proof, and `edge.id` is
optional.

The inner canvas has explicit pixel width and height. The SVG uses the same
dimensions and view box. HTML node buttons are positioned from
`LaidOutNode`; they are not placed inside SVG `foreignObject`.

### Node pill

Each 48px pill has two clamped lines:

1. `WorkflowStatusIcon`, primary identity (role, else formatted agent label,
   else Task label, else title), and the exact localized status text; and
2. `nodeDisplayTitle` (`title → summary → node_id`).

Long identity, status, and title text may truncate visually, but the full title
is available in the selected detail and in `title`/accessible text.

Expose these stable semantics:

- `data-status` for every `ProjectedNodeStatus` value;
- `data-estimated` from `isEstimatedNode(node)`, not from a hand-written subset
  of statuses;
- `data-current` for every ID in `snapshot.current_node_ids`;
- `data-selected` and `aria-pressed` for the single selected button;
- `data-sync-state` for `in_sync` / `out_of_sync`; and
- `focus-visible` ring independent of current/selected styling.

Use the existing `WorkflowStatusIcon` and `nodeStatus.*` mapping for all 13
status values. Estimated nodes use the dashed/inactive treatment. Current,
selected, blocked, and out-of-sync accents must remain distinguishable without
replacing status shape with color alone. An out-of-sync pill adds a compact
`AlertTriangleIcon` indicator and the localized accessible copy while retaining
its ordinary status icon.

### Selection reconciliation

Selection is panel-local and has an automatic vs user-selected state:

1. First verify that all node IDs are non-empty and unique. If not, clear
   selection and keep the duplicate-ID fallback non-interactive.
2. On mount, choose the first `current_node_ids` entry that exists in
   `snapshot.nodes`; otherwise select nothing.
3. Before the user selects a node, follow changes to that preferred current
   node on live snapshot refreshes.
4. After a user selects a node, keep it across snapshot refreshes while the ID
   still exists, even if `current_node_ids` changes.
5. If the selected node disappears, clear the user-dirty flag and fall back to
   the first valid current node, or none.
6. A selection-scope change (most importantly, switching conversations)
   remounts the keyed Simple child and resets user-dirty state even when the
   new snapshot reuses IDs such as `simple-task-1`.
7. Leaving the workflow segment, collapsing/unmounting the overlay, or changing
   compatibility unmounts the Simple panel and clears selection.

This avoids both stale details and a detail card that unexpectedly jumps away
from a user's explicit selection.

### Selected-node detail

When selection exists, render exactly one
`data-testid="workflow-dag-detail"` card immediately after the canvas or
fallback; otherwise render none. Give it a stable ID plus `role="region"` and
`aria-label={dagSelectedNode}`; do not make it an `aria-live` region because a
live elapsed value can update every second. It contains:

- full title, Task index, role, formatted agent, model, effort, and exact
  status;
- `simpleLiveRun` when the latest run is live;
- the existing operational line, including lineage elapsed time, tool count,
  and edit/file/line rollups when present;
- run/replacement/round counts when non-zero;
- `simpleOutOfSync` when `sync_state === "out_of_sync"`; and
- an Open conversation button when `canOpenWorkflowNode` succeeds;
  `estimatedNonActionable` for estimated nodes; or existing `noSessions` copy
  for an observed-but-not-openable defensive state.

The detail card does **not** repeat visible predecessor/successor text such as
“Depends on” or “Required by” (`依赖` / `解锁`). The graph lines remain the
visible topology carrier, while the edge-derived `aria-describedby` copy below
remains available to screen-reader users.

Activate the one-second `useNowMs` subscription only when the **selected** node
is live. CSS status pulses need no JavaScript timer.

Opening remains a separate action from selection. Clicking a pill only selects
it. The detail button uses `canOpenWorkflowNode` and preserves the current
Simple test ID:

```text
simple-task-open-{node_id}
```

It calls `openDelegatedChildSession` with the same child ID, agent type, and
title as the existing Simple list.

### Empty, unmeasured, and invalid states

| State | Rendering |
|---|---|
| `nodes.length === 0` | Existing `simpleNoTasks`; no canvas, fallback, or detail |
| width not yet positive | Mounted `workflow-dag-canvas` root with `aria-busy`; no inner SVG; not an error |
| valid nodes, `edges.length === 0` | All nodes in rank 0; no SVG paths |
| cycle, duplicate edge, dangling endpoint, or unsupported span | Dedicated graph warning plus source-order compact node fallback; measured root remains, but `workflow-dag-svg` is absent |
| duplicate/blank node ID | Dedicated graph warning and non-interactive node summary; no SVG or ambiguous current/selected semantics |

Use `data-testid="workflow-dag-error"` with `data-layout-error` for the
dedicated `role="status"` warning and `workflow-dag-fallback` for the compact
fallback. The fallback may expose the same selectable pills when node IDs are
unique; session opening still happens only through the panel-owned detail
action. It must never resurrect the Simple dependency list or draw a partial
topology. Because validation rejected the topology, fallback pills do not
attach edge-derived `aria-describedby` relationship text; presenting those
relations as trustworthy would contradict the warning. Name the list with
`dagFallbackAria`.

### What must not appear in Simple

- `workflow-graph-edges` or `workflow-dependencies-toggle`;
- `simple-task-row-*` after migration;
- manifest lane toggles;
- dummy gate chrome such as `Reviewer cohort` or fixture `0 / 1`;
- completion decision/history cards or Done/Retry actions; and
- empty `workflow-graph-lane-design|plan|final` sections.

## Data vocabulary preservation

For a valid graph, no information currently shown by the Simple list is lost:

| Snapshot/display vocabulary | New carrier |
|---|---|
| Task index, role/agent, exact status | pill + detail |
| title/summary fallback | pill + full detail |
| `current_node_ids` | current accent + accessible current-node text + automatic selection |
| `edges.from/to` | SVG path + per-node accessible relationships |
| Task current/total | Tasks header; node-count fallback for legacy Simple |
| live status | pill icon/status + detail live line |
| elapsed, tools, edits, file/line rollup | detail operational line |
| run/replacement/round counts | detail |
| `sync_state` and node warnings | pill accent/attributes + warning banner + detail |
| Plan/progress paths | existing file buttons |
| `overall_state` | existing overlay header badge |

Still intentionally omitted: raw work-unit keys, raw touched-file paths,
manifest gate/completion controls, and token/cost values.

## Internationalization

Add these keys under `Folder.chat.workflowGraph` in all ten locale files:

| Key | Purpose / required placeholder |
|---|---|
| `dagAria` | Canvas group name |
| `dagSelectedNode` | Selected-detail label |
| `dagCurrentNode` | Accessible current-node qualifier |
| `dagDependsOn` | Incoming relationship; contains `{nodes}` |
| `dagRequiredBy` | Outgoing relationship; contains `{nodes}` |
| `dagInvalidGraph` | Visible invalid-topology warning |
| `dagFallbackAria` | Compact no-topology fallback list name |

Reuse existing `phase.*`, `phaseStatus.*`, `nodeStatus.*`, `roleLabel`,
`agentLabel`, `modelLabel`, `effortLabel`, `taskIndex`, `runCount`,
`replacementCount`, `roundCount`, `openSession`, `estimatedNonActionable`, and
`noSessions`, plus all existing `simple*`/operational-stat keys. Do not
hardcode user-facing English or Chinese in components.

`src/i18n/messages.test.ts` already checks locale key parity. Extend its
workflow-key test to require every new key to be non-empty and verify the
`{nodes}` placeholders; this is not optional.

## Accessibility

- The valid scroll viewport is a named `role="group"`; node buttons appear in
  DOM order by rank and then logical inline-start order. In RTL, the first DOM
  sibling is mirrored to the right, so keyboard order still follows visual
  reading order.
- Use `aria-pressed`, not `aria-current`, for selection. Multiple workflow
  nodes can be current simultaneously, so `aria-current` would encode the wrong
  single-current semantic.
- Append localized `dagCurrentNode` and out-of-sync/status information to each
  button's accessible name as applicable.
- For every valid node, derive predecessor and successor title lists from
  `snapshot.edges`, join titles with the direction-neutral ` · ` separator,
  and attach localized `dagDependsOn` / `dagRequiredBy` text via
  `aria-describedby`. This avoids requiring `ES2021.Intl` in the repository's
  ES2020 TypeScript lib. Generate description IDs from a sanitized `useId`
  prefix plus source node index, not from raw node IDs. Visible arrows alone
  are not an accessible topology.
- The SVG and marker are `aria-hidden`; paths have no pointer target.
- The selected button uses `aria-controls` for the stable detail-card ID.
- Retain native buttons, visible focus, shape-plus-color status encoding, and
  the existing `motion-safe:` behavior in `WorkflowStatusIcon`.
- Titles use `dir="auto"`; geometry uses the app's locale direction. Never
  mirror the whole SVG or node layer with CSS transforms.

## Testing

### Pure layout tests

Create `src/lib/workflow-dag-layout.test.ts` with the seven-node/seven-edge
reference topology. Assert:

- ranks 0 through 5, with both Task-2 reviewers at rank 3 and two incoming
  edges to the Task-3 implementer;
- deterministic output and no input mutation;
- one global node width, expected `canvasWidth`/height, and centered rows;
- width behavior at 224, 288, and 448px, including the 130px and 148px
  two-sibling cases;
- an edgeless non-empty graph produces one rank and no paths;
- empty, invalid width, blank node ID, duplicate node, duplicate edge, dangling
  edge, self-loop/cycle, and unsupported long-edge fixtures return exact
  errors, including blank-before-duplicate-node, duplicate-before-dangling,
  and dangling-before-self-loop precedence for multiply invalid inputs;
- RTL node boxes are exact horizontal mirrors of LTR while rank/Y values stay
  equal; and
- edge paths are rebuilt from mirrored boxes and terminate above the target.

### Canvas and overlay tests

Add `simpleDagGraph()` rather than overloading the existing five-node fixture
where that would obscure intent. Provide a deterministic `ResizeObserver`
callback in the test. Assert:

- valid Simple renders `workflow-dag-canvas`, `workflow-dag-svg`, seven unique
  edge paths, and the Tasks section, with no old Simple rows or manifest
  lanes/dependency list;
- paths expose stable edge-index IDs and correct `data-from` / `data-to`;
- a pill click selects without opening; the detail Open action preserves
  `simple-task-open-*` and opens the correct child;
- first valid current ID is selected automatically, automatic selection tracks
  refreshes until user interaction, user selection remains sticky, and removal
  of the selected node falls back correctly;
- switching to another conversation that reuses the same Simple node IDs
  resets selection to that conversation's current node;
- all current nodes receive current semantics while exactly one node is
  selected;
- accessible predecessor/successor descriptions come from edges and name the
  related node titles;
- full title, status, live/out-of-sync copy, elapsed/tool/edit rollup,
  estimated non-actionable text, and the observed-without-child `noSessions`
  fallback appear in the detail as applicable;
- Plan/progress links, backend warning detection, and the existing Simple file
  watch tests still pass;
- fixture gates and completion projections never render in Simple;
- cycle/dangling/unsupported fixtures retain the measured root and render the
  dedicated warning and compact fallback, but no `workflow-dag-svg`; duplicate
  node IDs do not create ambiguous interactive buttons, and invalid fallback
  pills expose no edge-derived `aria-describedby`; and
- empty nodes use `simpleNoTasks`, while a non-empty edgeless graph is valid.

Keep the existing manifest/archived/observed-only overlay assertions as
regression coverage; do not rewrite them to the DAG contract.

### i18n tests

Assert locale parity, non-empty values for all seven new keys, and `{nodes}` in
both relationship strings for every locale.

### Verification commands for implementation

Focused feedback:

```bash
pnpm exec vitest run src/lib/workflow-dag-layout.test.ts \
  src/components/chat/workflow-overlay.test.tsx \
  src/i18n/messages.test.ts
pnpm exec eslint src/lib/workflow-dag-layout.ts \
  src/lib/workflow-dag-layout.test.ts \
  src/components/chat/workflow-dag-canvas.tsx \
  src/components/chat/workflow-graph-panel.tsx \
  src/components/chat/workflow-overlay.test.tsx \
  src/i18n/messages.test.ts
```

Final frontend regression:

```bash
pnpm eslint .
pnpm test
pnpm build
```

No Rust source changes or Rust test suite are required for this frontend-only
design.

Manual visual verification is required at 224, 288, and 448px in light/dark
themes and in English/Arabic. Check focus rings, horizontal overflow, arrowhead
direction, two-reviewer fan-out/fan-in, long translated status text, and outer
vertical scrolling.

## Files to touch

| File | Responsibility |
|---|---|
| `src/lib/workflow-dag-layout.ts` | Pure validation, ranks, geometry, direction, paths |
| `src/lib/workflow-dag-layout.test.ts` | Determinism, geometry, errors, RTL |
| `src/components/chat/workflow-dag-canvas.tsx` | Measurement, layered SVG/HTML canvas, a11y, fallback |
| `src/components/chat/workflow-graph-panel.tsx` | Compatibility split, Simple chrome/header/selection/detail; manifest behavior preserved |
| `src/components/chat/workflow-overlay.test.tsx` | Simple DAG interactions and manifest regressions |
| `src/i18n/messages/*.json` | Seven DAG/a11y strings in ten locales |
| `src/i18n/messages.test.ts` | Key, placeholder, and non-empty assertions |

No `src/lib/types.ts`, Rust, `workflow-graph-store`, or
`sub-agent-overlay.tsx` behavior change is planned. A test-only adjustment to
the overlay test's `ResizeObserver` setup is part of the listed test file.

## Acceptance criteria

1. At 224–448px, a valid routed Simple snapshot shows a top-to-bottom DAG with
   visible reviewer fan-out and next-Task fan-in.
2. Normal Simple rendering contains one Tasks section, no old flat rows, no
   dependency list, no manifest lanes, gates, or completion controls.
3. Node and edge placement comes only from snapshot nodes/edges; `deps`, role,
   and Task index never create an edge.
4. All current Simple information remains available in the pill/detail/header
   carriers listed above, without visible predecessor/successor or
   dependency/unlock prose in the selected detail.
5. Selection reconciliation follows the automatic/user-dirty contract, and
   opening a session remains a separate, correctly gated action.
6. Graph topology is conveyed through both visible arrows and per-node
   accessible predecessor/successor descriptions.
7. Resize and RTL recompute deterministic geometry without hardcoded overlay
   width or whole-layer transforms.
8. Invalid topology never throws or renders partial arrows; it shows the
   dedicated bounded fallback.
9. Manifest/observed-only UI and tests are unchanged.
10. Focused tests, full ESLint, full Vitest, and static-export build pass; the
    manual width/theme/RTL checklist is completed.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Actual viewport is narrower than stored overlay width | Measure the content box with `ResizeObserver` |
| A wide rank cannot fit | One global width shrinks to 96px, then horizontal scrolls |
| SVG nodes have poor keyboard/text behavior | SVG edges plus native HTML buttons |
| Edge IDs are absent or collide when concatenated | Source edge index is render identity/test ID |
| Long/transitive edge crosses a node | Reject unsupported rank span and show the no-topology fallback |
| Corrupt graph hides all useful node state | Preserve a compact node fallback whenever IDs remain unambiguous |
| Selection becomes stale during live refresh | Explicit automatic/user-dirty reconciliation |
| Screen readers cannot perceive arrows | Edge-derived `aria-describedby` relationships |
| Rejected topology is still announced as trustworthy in fallback | Omit edge-derived relationship descriptions from every invalid fallback |
| Live clock remains subscribed when no visible detail needs it | Subscribe only for the selected live node |
| Simple accidentally inherits fixture gates/completion | Strip gates before phase metadata and keep compatibility components split |
| RTL flips text or arrow semantics | Mirror coordinates first, then rebuild paths; never transform the whole layer |
| Manifest UI regresses during refactor | Separate child component and retain existing manifest tests unchanged |

## Later, not this design

- Reuse the canvas for manifest/observed-only graphs after phase bands, gates,
  archived completion, and long-edge routing have their own approved contract.
- Horizontal layout at a future wider host width.
- Crossing reduction, virtual/dummy vertices for transitive edges, edge labels,
  minimap, persisted selection/scroll, or graph virtualization.
- Token/cost rollups after the snapshot carries those values.

## Implementation sequencing

### PR 1: Pure layout

- Add the validation/rank/geometry module and its complete unit suite.
- No UI wiring and no snapshot/store changes.

### PR 2: Simple canvas and panel integration

- Add the layered canvas.
- Split the compatibility dispatcher from manifest rendering.
- Replace `SimpleWorkflowProjection` with the Tasks DAG and detail.
- Add i18n and overlay tests.
- Run focused and full frontend verification plus the manual visual checklist.

The detailed implementation plan is a separate post-approval artifact; these
two boundaries only establish reviewable dependency order.

## Design-review corrections incorporated

| Finding in the original draft | Resolution in this revision |
|---|---|
| Described a non-existent uncommitted Simple lane implementation | Rebased the design on the actual flat `SimpleWorkflowProjection` branch |
| Claimed a 448–768px expanded range | Corrected the real 224–448px overlay contract |
| Put buttons conceptually inside an SVG without defining how | Specified SVG edges plus an HTML button layer; no `foreignObject` |
| Layout output omitted `canvasWidth`, node width, and stable source identities | Added a complete discriminated output contract |
| Called 148px a minimum while shrinking below it | Split ideal width (148) from minimum width (96) and defined one graph-wide width |
| Per-rank shrink/centering and overflow formulas were ambiguous | Added graph-wide formulas and concrete 224/288/448 expectations |
| No real-width measurement or jsdom strategy | Added `ResizeObserver`, zero-width behavior, and deterministic test requirements |
| Error fallback contradicted itself and reused inaccurate projection copy | Added a dedicated error, no partial topology, and a bounded node fallback |
| Cycles/dangling edges were covered but invalid/duplicate IDs, duplicate edges, and long edges were not | Added explicit validation and exact error cases |
| Selection default had no live-refresh semantics | Added automatic vs user-dirty reconciliation |
| `aria-current` conflated selection with multiple current nodes | Replaced it with `aria-pressed` plus separate current-node semantics |
| Hidden SVG edges left topology inaccessible | Added edge-derived predecessor/successor descriptions |
| Selected detail repeated dependency/unlock prose already visible in the graph | Removed that visible detail copy while retaining edge-derived screen-reader descriptions on valid node buttons |
| Invalid fallback could expose relationships from a graph it had just rejected | Restricted edge-derived descriptions to valid layouts |
| Status and detail vocabulary was incomplete | Covered every status through shared mappings and preserved operational/edit/run data |
| Claimed the wrong existing Open-button test ID | Preserved the current Simple `simple-task-open-*` ID |
| Simple initialized manifest hooks and an extra live-clock subscription before the early return | Required a compatibility dispatcher with separate child components |
| i18n and tests were conditional or underspecified | Made keys, placeholders, resize, RTL, error, state, full regression, and manual checks normative |

## Open questions

None. The approved spec is ready for implementation planning.
