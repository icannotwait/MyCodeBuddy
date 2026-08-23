import { describe, expect, it } from "vitest"

import {
  ARROW_END_GAP,
  NODE_HEIGHT,
  layoutWorkflowDag,
} from "@/lib/workflow-dag-layout"
import type { WorkflowEdgeSnapshot, WorkflowNodeSnapshot } from "@/lib/types"

function node(
  node_id: string,
  task_index: number | null,
  role: string | null
): WorkflowNodeSnapshot {
  return {
    node_id,
    kind: "work_unit",
    phase_id: "tasks",
    role,
    agent_type: null,
    model: null,
    effort: null,
    profile_id: null,
    task_index,
    task_risk_level: null,
    task_risk_reason_codes: [],
    required_reviewer_count: null,
    returned_reviewer_count: null,
    title: node_id,
    status: "pending",
    sync_state: "in_sync",
    projection_warning_codes: [],
    status_reason: null,
    run_count: 0,
    active_child_generation: null,
    replacement_count: 0,
    gate_cycle: null,
    round_count: null,
    latest_task_id: null,
    latest_child_conversation_id: null,
    latest_run_status: null,
    started_at: null,
    finished_at: null,
    elapsed_completed_ms: null,
    tool_call_count: null,
    edit_tool_call_count: null,
    touched_file_count: null,
    touched_files_truncated: false,
    additions: null,
    deletions: null,
    line_counts_complete: null,
    summary: null,
    completion: null,
    is_observed: false,
    retained_observed: false,
    required: true,
    node_outcome: null,
    deps: [],
  }
}

function referenceGraph(): {
  nodes: WorkflowNodeSnapshot[]
  edges: WorkflowEdgeSnapshot[]
} {
  return {
    nodes: [
      node("t1-impl", 1, "implementer"),
      node("t1-primary", 1, "reviewer"),
      node("t2-impl", 2, "implementer"),
      node("t2-primary", 2, "reviewer"),
      node("t2-aux", 2, "reviewer"),
      node("t3-impl", 3, "implementer"),
      node("t3-primary", 3, "reviewer"),
    ],
    edges: [
      { id: "e-1", from: "t1-impl", to: "t1-primary" },
      { id: "e-2", from: "t1-primary", to: "t2-impl" },
      { id: "e-3", from: "t2-impl", to: "t2-primary" },
      { id: "e-4", from: "t2-impl", to: "t2-aux" },
      { id: "e-5", from: "t2-primary", to: "t3-impl" },
      { id: "e-6", from: "t2-aux", to: "t3-impl" },
      { from: "t3-impl", to: "t3-primary" },
    ],
  }
}

describe("layoutWorkflowDag", () => {
  it("lays out the routed graph in six deterministic ranks", () => {
    const graph = referenceGraph()
    const before = structuredClone(graph)
    const result = layoutWorkflowDag({
      ...graph,
      viewportWidth: 288,
      direction: "ltr",
    })
    expect(result.ok).toBe(true)
    if (!result.ok) return
    expect(graph).toEqual(before)
    expect(result.canvasWidth).toBe(288)
    expect(result.height).toBe(444)
    expect(result.nodes.map(({ nodeId, rank }) => [nodeId, rank])).toEqual([
      ["t1-impl", 0],
      ["t1-primary", 1],
      ["t2-impl", 2],
      ["t2-primary", 3],
      ["t2-aux", 3],
      ["t3-impl", 4],
      ["t3-primary", 5],
    ])
    expect(new Set(result.nodes.map((item) => item.width))).toEqual(
      new Set([130])
    )
    expect(result.nodes.find((item) => item.nodeId === "t2-primary")?.x).toBe(8)
    expect(result.nodes.find((item) => item.nodeId === "t2-aux")?.x).toBe(150)
    expect(result.edges).toHaveLength(7)
    expect(result.edges[6]).toMatchObject({ edgeIndex: 6, edgeId: null })
    expect(result.edges.filter((edge) => edge.to === "t3-impl")).toHaveLength(2)
    expect(
      layoutWorkflowDag({ ...graph, viewportWidth: 288, direction: "ltr" })
    ).toEqual(result)
  })

  it.each([
    [224, 98, 224],
    [288, 130, 288],
    [448, 148, 448],
  ] as const)(
    "uses one graph-wide node width at a %ipx viewport",
    (viewportWidth, expectedNodeWidth, expectedCanvasWidth) => {
      const result = layoutWorkflowDag({
        ...referenceGraph(),
        viewportWidth,
        direction: "ltr",
      })
      expect(result.ok).toBe(true)
      if (!result.ok) return
      expect(new Set(result.nodes.map((item) => item.width))).toEqual(
        new Set([expectedNodeWidth])
      )
      expect(result.canvasWidth).toBe(expectedCanvasWidth)
      expect(result.nodes.find((item) => item.nodeId === "t1-impl")?.x).toBe(
        (expectedCanvasWidth - expectedNodeWidth) / 2
      )
    }
  )

  it("sorts each rank by Task, exact role order, then source index", () => {
    const nodes = [
      node("task-2-impl", 2, "implementer"),
      node("task-1-review", 1, "reviewer"),
      node("task-1-other", 1, "fixer"),
      node("task-1-missing", 1, null),
      node("task-1-author", 1, "author"),
      node("task-1-impl", 1, "implementer"),
      node("unindexed-impl", null, "implementer"),
    ]
    const result = layoutWorkflowDag({
      nodes,
      edges: [],
      viewportWidth: 1200,
      direction: "ltr",
    })
    expect(result.ok).toBe(true)
    if (!result.ok) return
    expect(result.nodes.map((item) => item.nodeId)).toEqual([
      "task-1-impl",
      "task-1-author",
      "task-1-review",
      "task-1-other",
      "task-1-missing",
      "task-2-impl",
      "unindexed-impl",
    ])
  })

  it("uses the minimum width and expands the inner canvas for three siblings", () => {
    const result = layoutWorkflowDag({
      nodes: [node("a", 1, null), node("b", 2, null), node("c", 3, null)],
      edges: [],
      viewportWidth: 224,
      direction: "ltr",
    })
    expect(result).toMatchObject({ ok: true, canvasWidth: 328, edges: [] })
    if (!result.ok) return
    expect(result.nodes.map((item) => item.rank)).toEqual([0, 0, 0])
    expect(result.nodes.map((item) => item.width)).toEqual([96, 96, 96])
  })

  it("mirrors boxes and rebuilds edge paths for RTL", () => {
    const graph = referenceGraph()
    const ltr = layoutWorkflowDag({
      ...graph,
      viewportWidth: 288,
      direction: "ltr",
    })
    const rtl = layoutWorkflowDag({
      ...graph,
      viewportWidth: 288,
      direction: "rtl",
    })
    expect(ltr.ok && rtl.ok).toBe(true)
    if (!ltr.ok || !rtl.ok) return
    for (const left of ltr.nodes) {
      const right = rtl.nodes.find((item) => item.nodeId === left.nodeId)!
      expect(right.x).toBe(ltr.canvasWidth - left.x - left.width)
      expect(right.y).toBe(left.y)
      expect(right.rank).toBe(left.rank)
    }
    expect(rtl.edges[2].path).not.toBe(ltr.edges[2].path)
    const target = rtl.nodes.find((item) => item.nodeId === "t2-primary")!
    expect(
      rtl.edges[2].path.endsWith(
        `${target.x + target.width / 2} ${target.y - ARROW_END_GAP}`
      )
    ).toBe(true)
  })

  it("returns an edgeless non-empty graph as one centered rank", () => {
    const result = layoutWorkflowDag({
      nodes: [node("a", null, null), node("b", null, null)],
      edges: [],
      viewportWidth: 288,
      direction: "ltr",
    })
    expect(result.ok).toBe(true)
    if (!result.ok) return
    expect(result.nodes.map((item) => item.rank)).toEqual([0, 0])
    expect(result.edges).toEqual([])
    expect(result.height).toBe(2 * 8 + NODE_HEIGHT)
  })

  it.each([
    [
      "empty",
      [] as WorkflowNodeSnapshot[],
      [] as WorkflowEdgeSnapshot[],
      288,
      "empty",
    ],
    ["invalid width", [node("a", null, null)], [], 0, "invalid_width"],
    ["blank node id", [node("   ", null, null)], [], 288, "invalid_node_id"],
    [
      "blank node id before duplicate node",
      [node("a", null, null), node("a", null, null), node("   ", null, null)],
      [],
      288,
      "invalid_node_id",
    ],
    [
      "duplicate node id",
      [node("a", null, null), node("a", null, null)],
      [],
      288,
      "duplicate_node",
    ],
    [
      "duplicate edge",
      [node("a", null, null), node("b", null, null)],
      [
        { from: "a", to: "b" },
        { id: "different-id", from: "a", to: "b" },
      ],
      288,
      "duplicate_edge",
    ],
    [
      "duplicate edge before dangling endpoint",
      [node("b", null, null)],
      [
        { from: "missing", to: "b" },
        { from: "missing", to: "b" },
      ],
      288,
      "duplicate_edge",
    ],
    [
      "dangling edge",
      [node("a", null, null)],
      [{ from: "a", to: "missing" }],
      288,
      "dangling_edge",
    ],
    [
      "dangling edge before self loop",
      [node("a", null, null)],
      [
        { from: "a", to: "a" },
        { from: "a", to: "missing" },
      ],
      288,
      "dangling_edge",
    ],
    [
      "self loop",
      [node("a", null, null)],
      [{ from: "a", to: "a" }],
      288,
      "cycle",
    ],
    [
      "cycle",
      [node("a", null, null), node("b", null, null)],
      [
        { from: "a", to: "b" },
        { from: "b", to: "a" },
      ],
      288,
      "cycle",
    ],
    [
      "long edge",
      [node("a", null, null), node("b", null, null), node("c", null, null)],
      [
        { from: "a", to: "b" },
        { from: "b", to: "c" },
        { from: "a", to: "c" },
      ],
      288,
      "unsupported_edge_span",
    ],
  ] as const)("returns $4 for $0", (_name, nodes, edges, width, error) => {
    expect(
      layoutWorkflowDag({
        nodes,
        edges,
        viewportWidth: width,
        direction: "ltr",
      })
    ).toEqual({ ok: false, error })
  })
})
