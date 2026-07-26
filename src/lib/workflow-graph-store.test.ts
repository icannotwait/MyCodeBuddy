import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

import type { WorkflowGraphSnapshot, WorkflowNodeSnapshot } from "@/lib/types"

const {
  getWorkflowGraphSnapshot,
  subscribeWorkflowGraphChanged,
  subscribeWorkflowCompatibilityNudge,
} = vi.hoisted(() => ({
  getWorkflowGraphSnapshot: vi.fn(),
  subscribeWorkflowGraphChanged: vi.fn(async () => () => {}),
  subscribeWorkflowCompatibilityNudge: vi.fn(async () => () => {}),
}))

vi.mock("@/lib/api", () => ({
  getWorkflowGraphSnapshot: (...args: unknown[]) =>
    getWorkflowGraphSnapshot(...args),
  subscribeWorkflowGraphChanged: (...args: unknown[]) =>
    subscribeWorkflowGraphChanged(...args),
  subscribeWorkflowCompatibilityNudge: (...args: unknown[]) =>
    subscribeWorkflowCompatibilityNudge(...args),
}))

import {
  __resetWorkflowGraphStoreForTests,
  buildPhaseRail,
  canOpenWorkflowNode,
  compactRequiredGateCounts,
  isEstimatedNode,
  useWorkflowGraphStore,
} from "./workflow-graph-store"

function baseSnapshot(
  overrides: Partial<WorkflowGraphSnapshot> = {}
): WorkflowGraphSnapshot {
  return {
    schema_version: 1,
    workflow_id: "wf-1",
    workflow_kind: "brainstorm_to_delivery",
    manifest_revision: 1,
    graph_revision: 1,
    manifest_state: "estimated",
    compatibility: "manifest",
    overall_state: "estimated",
    current_phase_id: "plan",
    current_node_ids: ["n-plan-r1"],
    phases: [
      { id: "design", kind: "design", title: "Design" },
      { id: "plan", kind: "plan", title: "Plan" },
      { id: "tasks", kind: "tasks", title: "Tasks" },
      { id: "final", kind: "final", title: "Final" },
    ],
    nodes: [
      node({
        node_id: "n-plan-r1",
        phase_id: "plan",
        role: "reviewer",
        status: "running",
        required: true,
        is_observed: true,
        latest_child_conversation_id: 42,
      }),
      node({
        node_id: "n-plan-r2",
        phase_id: "plan",
        role: "reviewer",
        status: "estimated",
        required: true,
        is_observed: false,
      }),
      node({
        node_id: "n-plan-opt",
        phase_id: "plan",
        role: "reviewer",
        status: "completed",
        required: false,
        is_observed: true,
        latest_child_conversation_id: 99,
      }),
    ],
    edges: [],
    gates: [
      {
        gate_id: "g-plan",
        gate_kind: "plan",
        resolution_mode: "parent_adjudication",
        required_reviewer_node_ids: ["n-plan-r1", "n-plan-r2"],
        required_count: 2,
        returned_count: 0,
        running_count: 1,
        blocked_count: 0,
      },
    ],
    ...overrides,
  }
}

function node(
  overrides: Partial<WorkflowNodeSnapshot> & { node_id: string }
): WorkflowNodeSnapshot {
  return {
    kind: "work_unit",
    phase_id: null,
    role: null,
    agent_type: null,
    profile_id: null,
    task_index: null,
    title: overrides.node_id,
    status: "estimated",
    status_reason: null,
    run_count: 0,
    active_child_generation: null,
    replacement_count: 0,
    gate_cycle: null,
    round_count: null,
    latest_task_id: null,
    latest_child_conversation_id: null,
    latest_run_status: null,
    summary: null,
    is_observed: false,
    retained_observed: false,
    required: true,
    node_outcome: null,
    deps: [],
    ...overrides,
  }
}

beforeEach(() => {
  __resetWorkflowGraphStoreForTests()
  getWorkflowGraphSnapshot.mockReset()
  subscribeWorkflowGraphChanged.mockReset()
  subscribeWorkflowCompatibilityNudge.mockReset()
  subscribeWorkflowGraphChanged.mockResolvedValue(() => {})
  subscribeWorkflowCompatibilityNudge.mockResolvedValue(() => {})
})

afterEach(() => {
  __resetWorkflowGraphStoreForTests()
})

describe("workflow-graph-store revision gate", () => {
  it("applies detail snapshot and discards stale lower graph_revision", () => {
    const store = useWorkflowGraphStore.getState()
    store.applyFromDetail(7, baseSnapshot({ graph_revision: 3 }))
    expect(store.getSnapshot(7)?.graph_revision).toBe(3)

    store.applyFromDetail(7, baseSnapshot({ graph_revision: 2 }))
    expect(useWorkflowGraphStore.getState().getSnapshot(7)?.graph_revision).toBe(
      3
    )

    store.applyFromDetail(7, baseSnapshot({ graph_revision: 5 }))
    expect(useWorkflowGraphStore.getState().getSnapshot(7)?.graph_revision).toBe(
      5
    )
  })

  it("discards fetched snapshot when request generation is stale", async () => {
    const resolvers: Array<(v: WorkflowGraphSnapshot | null) => void> = []
    getWorkflowGraphSnapshot.mockImplementation(
      () =>
        new Promise<WorkflowGraphSnapshot | null>((resolve) => {
          resolvers.push(resolve)
        })
    )

    const unmount = useWorkflowGraphStore.getState().mountConversation(11)
    useWorkflowGraphStore.getState().applyFromDetail(
      11,
      baseSnapshot({ graph_revision: 1 })
    )

    // First nudge starts gen=1 fetch.
    useWorkflowGraphStore.getState().handleCompatibilityNudge({
      parent_conversation_id: 11,
    })
    expect(
      useWorkflowGraphStore.getState().getEntry(11)?.requestGeneration
    ).toBe(1)
    await vi.waitFor(() => expect(resolvers.length).toBe(1))

    // Second nudge supersedes before first resolves (gen=2).
    useWorkflowGraphStore.getState().handleCompatibilityNudge({
      parent_conversation_id: 11,
    })
    expect(
      useWorkflowGraphStore.getState().getEntry(11)?.requestGeneration
    ).toBe(2)
    await vi.waitFor(() => expect(resolvers.length).toBe(2))

    // Resolve the first (stale gen=1) fetch with a shiny revision — must discard.
    resolvers[0](baseSnapshot({ graph_revision: 99, workflow_id: "stale" }))
    await Promise.resolve()
    await Promise.resolve()

    expect(
      useWorkflowGraphStore.getState().getSnapshot(11)?.graph_revision
    ).toBe(1)
    expect(useWorkflowGraphStore.getState().getSnapshot(11)?.workflow_id).toBe(
      "wf-1"
    )

    // Resolve the second (current gen=2) fetch.
    resolvers[1](baseSnapshot({ graph_revision: 4, workflow_id: "fresh" }))
    await Promise.resolve()
    await Promise.resolve()

    expect(
      useWorkflowGraphStore.getState().getSnapshot(11)?.graph_revision
    ).toBe(4)
    expect(useWorkflowGraphStore.getState().getSnapshot(11)?.workflow_id).toBe(
      "fresh"
    )

    unmount()
  })

  it("graph_changed with lower-or-equal revision does not refetch", async () => {
    getWorkflowGraphSnapshot.mockResolvedValue(
      baseSnapshot({ graph_revision: 10 })
    )
    useWorkflowGraphStore.getState().applyFromDetail(
      3,
      baseSnapshot({ graph_revision: 5 })
    )
    useWorkflowGraphStore.getState().mountConversation(3)

    useWorkflowGraphStore.getState().handleGraphChanged({
      parent_conversation_id: 3,
      workflow_id: "wf-1",
      graph_revision: 5,
    })
    useWorkflowGraphStore.getState().handleGraphChanged({
      parent_conversation_id: 3,
      workflow_id: "wf-1",
      graph_revision: 4,
    })
    expect(getWorkflowGraphSnapshot).not.toHaveBeenCalled()

    useWorkflowGraphStore.getState().handleGraphChanged({
      parent_conversation_id: 3,
      workflow_id: "wf-1",
      graph_revision: 6,
    })
    await vi.waitFor(() => {
      expect(getWorkflowGraphSnapshot).toHaveBeenCalledWith(3)
    })
    await vi.waitFor(() => {
      expect(
        useWorkflowGraphStore.getState().getSnapshot(3)?.graph_revision
      ).toBe(10)
    })
  })

  it("compatibility_nudge refetches with local request generation", async () => {
    getWorkflowGraphSnapshot.mockResolvedValue(
      baseSnapshot({
        graph_revision: null,
        compatibility: "observed_only",
        overall_state: "observed_only",
        workflow_id: null,
      })
    )
    const unmount = useWorkflowGraphStore.getState().mountConversation(9)
    useWorkflowGraphStore.getState().handleCompatibilityNudge({
      parent_conversation_id: 9,
    })
    await vi.waitFor(() => {
      expect(
        useWorkflowGraphStore.getState().getSnapshot(9)?.compatibility
      ).toBe("observed_only")
    })
    expect(
      useWorkflowGraphStore.getState().getEntry(9)?.requestGeneration
    ).toBeGreaterThan(0)
    unmount()
  })
})

describe("B11 compact required counts", () => {
  it("uses gate required_count / returned_count only (not optional nodes)", () => {
    const snap = baseSnapshot()
    // Optional reviewer is completed but must not appear in denominator.
    const counts = compactRequiredGateCounts(snap, "plan")
    expect(counts).toEqual({
      returned: 0,
      required: 2,
      running: 1,
      blocked: 0,
    })
    // Sanity: nodes include an optional completed reviewer.
    expect(snap.nodes.filter((n) => !n.required)).toHaveLength(1)
  })

  it("falls back to required nodes only when gate row missing for design/plan", () => {
    const snap = baseSnapshot({ gates: [] })
    const counts = compactRequiredGateCounts(snap, "plan")
    expect(counts?.required).toBe(2)
    expect(counts?.returned).toBe(0)
    // Optional completed does not count as returned.
    expect(counts?.running).toBe(1)
  })
})

describe("node openability / estimated", () => {
  it("marks estimated nodes non-openable", () => {
    const estimated = node({
      node_id: "e1",
      status: "estimated",
      is_observed: false,
      latest_child_conversation_id: null,
    })
    expect(isEstimatedNode(estimated)).toBe(true)
    expect(canOpenWorkflowNode(estimated)).toBe(false)
  })

  it("allows observed nodes with child conversation id", () => {
    const observed = node({
      node_id: "o1",
      status: "running",
      is_observed: true,
      latest_child_conversation_id: 55,
    })
    expect(isEstimatedNode(observed)).toBe(false)
    expect(canOpenWorkflowNode(observed)).toBe(true)
  })
})

describe("phase rail", () => {
  it("builds four fixed phases and attaches B11 gate on plan", () => {
    const rail = buildPhaseRail(baseSnapshot())
    expect(rail.map((p) => p.kind)).toEqual([
      "design",
      "plan",
      "tasks",
      "final",
    ])
    const plan = rail.find((p) => p.kind === "plan")
    expect(plan?.gate?.required).toBe(2)
    expect(plan?.status).toBe("current")
  })
})
