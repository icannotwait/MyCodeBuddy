# Simple Workflow DAG Overlay Design

**Date:** 2026-08-23
**Status:** Draft for review. Layout locked in conversation (vertical
rank DAG inside the Tasks lane; Plan/progress chrome kept; selected-node
detail card; no "依赖 (N)" list). Do not implement until this spec is
approved.
**Scope:** Frontend overlay Graph for `compatibility === "simple"`
snapshots. No backend, types, store, or Skill contract changes.
**Supersedes (narrowly):** the "连线图可视化" item listed as out of
scope in
`docs/superpowers/specs/2026-07-30-workflow-overlay-ui-design.md`.
Lane grouping, status icons, Plan/progress links, and redacted snapshot
vocabulary stay.

## Problem

Simple brainstorm-to-delivery is the only writable workflow path
(`2026-08-11-simple-workflow-v2-retirement-design.md`). The overlay
therefore has to explain a **plan-driven DAG** (implementer → reviewers
→ next implementer) from a `WorkflowGraphSnapshot`.

Two UI shapes already failed that job:

1. **Flat Simple list** (`SimpleWorkflowProjection`). Seven work-unit
   rows with no topology. Expand-graph is hidden when
   `compatibility === "simple"`.
2. **Phase lanes + "依赖 (N)" text list** (current `WorkflowGraphPanel`
   after a follow-up). Reviewer cards nest under implementers, and edges
   become `titleA → titleB` rows. That is an adjacency list, not a
   directed graph: fan-out and fan-in are not spatial.

The snapshot already has the graph. `project_simple_mode` emits
`nodes[]` and `edges[]` (Simple demo: 7 nodes, 7 edges). The product
gap is rendering.

Default overlay width is 288px (drag 224–448px; expanded graph
448–768px). A left-to-right six-rank DAG does not fit. The locked
layout is **top-to-bottom ranks** with same-rank siblings side by side.

## Decision

Replace the Simple Graph body (stacked role cards + dependency list)
with an SVG node-link DAG driven only by `snapshot.nodes` and
`snapshot.edges`. Keep overlay chrome. Keep a single selected-node
detail card under the canvas. Do not add a graph library.

```text
SubAgentOverlay  (workflow segment, Simple)
  Plan / progress file links
  partial-projection warning (unchanged)
  Tasks lane header (status + 任务 current/total)
    WorkflowDagCanvas
      rank rows from longest-path layout
      SVG edges (fan-out / fan-in)
      node pills (status icon + role/agent + status)
  selected node detail card (one at a time)
```

Manifest (`compatibility !== "simple"`) expanded Graph stays on the
2026-07-30 lane + dependency-list contract in this spec's v1. Reuse of
the canvas there is a later PR, not a silent coupling.

## Goals

1. A Simple workflow overlay shows a **spatial DAG**: nodes as pills,
   edges as arrows, Task-2 reviewer fan-out and Task-3 fan-in visible
   without opening a text list.
2. Layout uses only snapshot `nodes` / `edges`. No inferred edges from
   `task_index` or role.
3. Overlay chrome is unchanged: 工作流/会话 tabs, overall state,
   打开 Plan / 打开进度, partial-projection warning, Simple file watch.
4. Clicking a DAG node selects it and shows **one** detail card.
   `canOpenWorkflowNode` still opens the child session.
5. Empty Design / Plan / Final lanes stay omitted for Simple.
6. Cycles, missing endpoints, and empty graphs fail closed (no throw).

## Non-Goals

- Backend projection, manifest publish, gates, completion protocol.
- A general graph editor, pan/zoom map, or third-party graph library.
- Changing Simple Plan/progress documents or `register_simple_workflow`.
- Persisting DAG selection or canvas scroll.
- Virtualizing 50+ nodes in v1.
- Replacing the compact phase rail on **manifest** conversations.
- Token / elapsed cost surfaces (still absent from the snapshot).

## Product choices (locked)

| Choice | Value |
|--------|--------|
| Canvas placement | Inside the Tasks lane, below the lane header |
| Direction | Top → bottom (ranks). Same rank = one horizontal row |
| Dependency list | Removed for Simple. `workflow-graph-edges` is not rendered |
| Stacked role cards | Removed for Simple. Pills on the canvas replace them |
| Detail | One card under the canvas for `selectedNodeId` |
| Expand toggle | Still hidden for Simple. The DAG **is** the Graph body |
| Empty phases | Simple renders only lanes with `nodeRows.length > 0` |
| Library | None. Focused React + SVG |
| Manifest v1 | Unchanged lane + "依赖 (N)" list |

Approved conversation mock: vertical T1 impl → T1 review → T2 impl
(current) → {T2 primary, T2 auxiliary} → T3 impl → T3 review.

## Key Decisions

1. **Edges are the topology authority.** Rank and arrows come from
   `snapshot.edges`. `task_index` / `role` only affect pill labels and
   stable sort inside a rank. Inventing edges from "next task" would
   disagree with the projector (high-risk tasks already emit two
   reviewer edges and a dual fan-in).
2. **Longest-path ranks, not a library layout.** Simple B2D graphs are
   small, layered, and already acyclic. Longest path from sources gives
   a unique rank per node and places fan-out on one row. Sugiyama
   crossing reduction is unnecessary at this size.
3. **Vertical ranks because the overlay is narrow.** Horizontal
   six-column layout was rejected in review of the live 288px panel.
4. **Simple-only in v1.** Manifest conversations still need Design /
   Plan / Final lanes and gate chrome from 2026-07-30. Sharing the
   canvas before that contract is ported would drop gate progress.
5. **Fail closed on cycles.** Production Simple snapshots are DAGs.
   If `edges` contain a cycle or a dangling endpoint, keep the current
   lane/card body (or an empty-canvas message if that body is already
   gone) and do not throw. Tests cover both the happy DAG and the
   cycle fallback.
6. **Selection is panel-local.** `useState` in `WorkflowGraphPanel`.
   Unmount (leave workflow segment / collapse overlay) clears it.
   Default selection: first `current_node_ids` hit, else none.

## Layout algorithm (frozen)

Pure helper `src/lib/workflow-dag-layout.ts`. No DOM. Input:
`nodes: WorkflowNodeSnapshot[]`, `edges: {from, to}[]`,
`width: number`. Output: `{ nodes: LaidOutNode[], edges: LaidOutEdge[],
height: number }` or `{ error: "cycle" | "dangling" | "empty" }`.

### Graph construction

- Node set = `snapshot.nodes` keyed by `node_id`.
- Ignore an edge if `from` or `to` is missing (count as dangling if any
  such edge exists; still layout the remaining well-formed subgraph
  **unless** the caller requested fail-closed — v1 **fails closed** and
  returns `error: "dangling"` when any edge is invalid).
- Ignore self-loops as `error: "cycle"`.

### Cycle detection

Kahn's algorithm on the well-formed subgraph. If not all nodes are
emitted, return `error: "cycle"`. Isolated nodes are valid (rank 0).

### Rank

```
rank(n) = 0  if indegree(n) == 0
rank(n) = 1 + max(rank(p) for p in predecessors(n))
```

Compute in topological order after Kahn.

### Sort inside a rank

1. `task_index` ascending (`null` last)
2. role order: `implementer`, `author`, `reviewer`, other, `null`
3. original `nodes` array order (stable)

### Geometry (logical pixels, LTR)

Constants (freeze; tests assert them):

| Token | Value |
|-------|-------|
| `NODE_MIN_WIDTH` | 148 |
| `NODE_HEIGHT` | 48 |
| `NODE_GAP_X` | 12 |
| `RANK_GAP_Y` | 28 |
| `PAD_X` | 8 |
| `PAD_Y` | 8 |

For rank `r` with `k` nodes:

- `rowWidth = k * NODE_MIN_WIDTH + (k - 1) * NODE_GAP_X`
- If `rowWidth + 2 * PAD_X > width`, shrink `NODE_MIN_WIDTH` equally
  down to 96px, then allow horizontal scroll (`canvasWidth = max(width,
  rowWidth + 2 * PAD_X)`).
- Center the row in `canvasWidth` when it is narrower than `width`.
- `y(r) = PAD_Y + r * (NODE_HEIGHT + RANK_GAP_Y)`
- Node `i` in the sorted rank: `x = startX + i * (nodeWidth + NODE_GAP_X)`

`height = PAD_Y * 2 + rankCount * NODE_HEIGHT + (rankCount - 1) * RANK_GAP_Y`

### Edge geometry

For each well-formed edge `from → to`:

- Start: bottom-center of `from` (`x + nodeWidth/2`, `y + NODE_HEIGHT`)
- End: top-center of `to` (`x + nodeWidth/2`, `y`) minus arrowhead pad
  (8px)
- Path: cubic bezier, control points at mid-Y between ranks so a
  one-to-two fan-out and two-to-one fan-in do not overlap the pills
- Arrowhead at `to` (`marker-end`)

RTL: compute LTR coordinates then mirror `x' = canvasWidth - x -
nodeWidth` for nodes; rebuild edge paths from mirrored boxes. Do not
hardcode `left`/`right`.

### Current / selected chrome (not in the pure helper)

The canvas applies CSS after layout:

- `snapshot.current_node_ids` → blue 1.8px stroke (`data-current`)
- `selectedNodeId` → 2px ring (`aria-current="true"`)
- `status === "pending" | "estimated"` → dashed border
- `status === "completed"` → solid + existing status icon
- `status === "running" | "reserving"` → pulse dot via
  `WorkflowStatusIcon` (`motion-safe:`)

## UI contract

### Chrome (unchanged)

- `data-testid="workflow-graph-panel"` and
  `data-compatibility="simple"`
- `simple-file-links`, `simple-progress-link`, Open Plan / Open Progress
- `simple-projection-warning` when locator missing **or** any
  projection warning on the snapshot or a node
- Live-run line and out-of-sync copy remain on the **detail card**,
  not on every pill (pills stay two lines: title + status)

### Tasks lane

- `data-testid="workflow-graph-lane-tasks"`
- Header: `WorkflowStatusIcon` + `phase.tasks` + `phaseStatus.*` +
  `phaseProgressFragments` (任务 current/total when implementer roles
  exist)
- Body: `WorkflowDagCanvas`, not stacked `workflow-graph-node-*` cards

### Canvas

| Test id | Role |
|---------|------|
| `workflow-dag-canvas` | Scroll container + SVG |
| `workflow-dag-node-{node_id}` | Pill button |
| `workflow-dag-edge-{from}-{to}` | One path (and the arrow marker) |

Pill content (two lines, `line-clamp-1` each):

1. Status icon + `roleLabel` or agent label
2. `nodeDisplayTitle` (existing helper: title → summary → node_id)

`aria-label` concatenates phase, task index, role, agent, status,
title (same ingredients as today's graph node `accessibleName`).

### Selection and open

- Click pill → `selectedNodeId = node_id`
- Default: `current_node_ids[0]` if that id exists in `nodes`
- Detail card `data-testid="workflow-dag-detail"` under the canvas:
  status icon, role, status badge, full title, agent, live-run /
  out-of-sync / operational line (elapsed · tools) when present
- Open control: same `canOpenWorkflowNode` rule, test id
  `workflow-graph-node-open-{node_id}` (keep the existing id so
  overlay tests that open a live child stay stable)

### Empty / error

| Snapshot | Render |
|----------|--------|
| `nodes.length === 0` | Existing `simpleNoTasks` copy; no canvas |
| layout `error` | `simplePartialProjection` warning (reuse) + no canvas; do **not** resurrect the dependency list |
| `edges.length === 0` but nodes exist | Rank-0 row of all nodes, no paths |

### What must not appear (Simple)

- `workflow-graph-edges` / `workflow-dependencies-toggle`
- `simple-task-row-*`
- Dummy gate chrome (`Reviewer cohort`, `0 / 1` from fixture gates)
- Completion decision cards (`Decision required`, Done / Retry)
- Empty `workflow-graph-lane-design|plan|final`

## Interaction with the uncommitted lane work

Working tree currently paints Simple snapshots with phase lanes plus
the dependency list. That was an intermediate attempt. This spec
**replaces** that body. Implementation should keep:

- Simple file-link chrome
- `data-compatibility="simple"`
- Stripping `gates` before `buildPhaseRail` so dummy gates cannot
  leak progress text
- Omitting empty phases

and should **not** keep the Simple dependency list.

## Data vocabulary preservation

Still shown, new carrier:

| Vocabulary | Carrier |
|------------|---------|
| node title / role / agent / status | DAG pill + detail |
| current_node_ids | pill stroke + default selection |
| edges from/to | SVG paths |
| task current/total | Tasks lane header |
| live elapsed / tool counts | detail card only |
| Plan / progress paths | file buttons |
| projection warnings / out_of_sync | warning banner + detail |
| overall_state | overlay header badge |

Still omitted (Simple projector already omits): work_unit_key, raw
paths in pills, completion protocol cards, document gates.

## i18n

Ten locales in `src/i18n/messages/*.json`, under
`Folder.chat.workflowGraph`:

| Key | Purpose |
|-----|---------|
| `dagAria` | Canvas `aria-label` (e.g. "Workflow DAG") |
| `dagSelectedNode` | Detail kicker ("Selected node") |

Reuse `phase.*`, `phaseStatus.*`, `nodeStatus.*`, `roleLabel`,
`agentLabel`, `taskIndex`, `openSession`, `simpleLiveRun`,
`simpleOutOfSync`, `simplePartialProjection`, `simpleNoTasks`,
`simpleOpenPlan`, `simpleOpenProgress`. No hardcoded Chinese/English
in components.

## Accessibility

- Canvas is a `role="group"` with `dagAria`. Pills are buttons in DOM
  order = rank order, then in-rank sort (keyboard matches visual
  top-to-bottom, then start-to-end).
- Selected pill: `aria-current="true"`.
- Edges are `aria-hidden`; topology is in each pill's `aria-label` plus
  the visible arrows.
- Status: existing shape + color via `WorkflowStatusIcon`.
- `prefers-reduced-motion`: `motion-safe:` on running pulse.
- RTL: mirrored geometry, not CSS `scaleX` on the whole SVG (that would
  flip text).

## Testing

No full desktop `cargo test` for this spec. Frontend:

### Layout (`src/lib/workflow-dag-layout.test.ts`)

Use the live demo topology (7 nodes / 7 edges):

```
t1-impl → t1-rev → t2-impl → t2-primary
                         ↘ t2-aux
t2-primary → t3-impl ← t2-aux
t3-impl → t3-rev
```

Assert:

- ranks: t1-impl 0, t1-rev 1, t2-impl 2, both t2 reviewers 3,
  t3-impl 4, t3-rev 5
- two nodes at rank 3; t3-impl has two incoming edges
- cycle fixture returns `error: "cycle"`
- edge to missing id returns `error: "dangling"`
- empty nodes returns `error: "empty"`
- width 288 vs 448: rank-3 pair stays one row; at 288 it may shrink
  node width but must not wrap to two ranks

### Overlay (`workflow-overlay.test.tsx`)

Extend `simpleGraph()` with implementer/reviewer roles matching the
demo (or add `simpleDagGraph()`). Assert:

- `workflow-dag-canvas` visible; `simple-task-row-*` absent
- `workflow-graph-edges` absent
- `workflow-graph-lane-tasks` present; design/plan/final lanes absent
- seven `workflow-dag-edge-*` (or the demo's edge count)
- file links and partial warning still work
- live child still opens via `workflow-graph-node-open-{id}`
- clicking a pending pill shows `workflow-dag-detail` with that title
- completion / gate strings still absent
- layout error fixture shows the partial warning and no canvas

### i18n

Existing `i18n/messages.test.ts` pattern: new keys present in all ten
locales, `dagAria` non-empty.

Verification commands for the implementation:

```bash
pnpm exec vitest run src/lib/workflow-dag-layout.test.ts \
  src/components/chat/workflow-overlay.test.tsx \
  src/i18n/messages.test.ts
pnpm exec eslint src/lib/workflow-dag-layout.ts \
  src/components/chat/workflow-dag-canvas.tsx \
  src/components/chat/workflow-graph-panel.tsx \
  src/components/chat/workflow-overlay.test.tsx
```

## Files to touch

| File | Role |
|------|------|
| `src/lib/workflow-dag-layout.ts` | Pure rank + geometry |
| `src/lib/workflow-dag-layout.test.ts` | Topology / error cases |
| `src/components/chat/workflow-dag-canvas.tsx` | SVG + pills |
| `src/components/chat/workflow-graph-panel.tsx` | Simple body = chrome + lane + canvas + detail |
| `src/components/chat/workflow-overlay.test.tsx` | Overlay assertions |
| `src/i18n/messages/*.json` | `dagAria`, `dagSelectedNode` |
| `src/i18n/messages.test.ts` | Key presence if the suite lists workflow keys |

No Rust, no `workflow-graph-store` API change (`buildPhaseRail` still
feeds the Tasks header). No new snapshot fields.

## Risks

| Risk | Mitigation |
|------|------------|
| 288px cannot fit two reviewer pills | Shrink to 96px then horizontal scroll; test both widths |
| Long titles overflow pills | `line-clamp-1` + `title` tooltip; full text on detail |
| Cycle in a corrupt snapshot | Fail closed, warning, no throw |
| Manifest accidentally loses lanes | `isSimple` branch only; existing tests for archived / v2 stay |
| Live elapsed clock on many pills | Clock stays on detail / running current node only |
| RTL flipped glyphs | Mirror positions, not the SVG group |

## Later, not this spec

- Reuse `WorkflowDagCanvas` as the manifest expanded Graph (would need
  Design/Plan/Final ranks or phase bands).
- Horizontal layout when overlay width ≥ 640px.
- Edge labels, minimap, persist selection.
- Cost / token rollup on the DAG (snapshot still has no tokens).

## PR Plan

### PR 1: Layout helper

- **Title:** Add Simple workflow DAG rank layout
- **Files:** `src/lib/workflow-dag-layout.ts`,
  `src/lib/workflow-dag-layout.test.ts`
- **Dependencies:** none
- **Description:** Frozen rank/geometry/error API with the 7-edge demo
  fixture and cycle/dangling cases. No UI.

### PR 2: Overlay canvas

- **Title:** Render Simple overlay as a vertical DAG
- **Files:** `workflow-dag-canvas.tsx`, `workflow-graph-panel.tsx`,
  overlay tests, i18n messages
- **Dependencies:** PR 1
- **Description:** Simple Graph body becomes Tasks lane + SVG DAG +
  selected detail. Remove Simple dependency list and stacked cards.
  Manifest lane UI unchanged.

## Open Questions

None. Layout, Simple-only v1, and removal of the dependency list were
locked in conversation on 2026-08-23.
