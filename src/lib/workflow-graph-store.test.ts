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

// Pass hoisted mocks through directly — do not re-wrap with `...args: unknown[]`
// spreads (TS2556: spread of unknown[] is not a rest tuple).
vi.mock("@/lib/api", () => ({
  getWorkflowGraphSnapshot,
  subscribeWorkflowGraphChanged,
  subscribeWorkflowCompatibilityNudge,
}))

import {
  __getWorkflowGraphEventInstallCounterForTests,
  __getWorkflowGraphEventInstallGenerationForTests,
  __resetWorkflowGraphStoreForTests,
  buildPhaseRail,
  canOpenWorkflowNode,
  compactRequiredGateCounts,
  computeTaskPhaseProgress,
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
    task_risk_level: null,
    task_risk_reason_codes: [],
    required_reviewer_count: null,
    returned_reviewer_count: null,
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

type Deferred<T> = {
  promise: Promise<T>
  resolve: (value: T) => void
  reject: (reason?: unknown) => void
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void
  let reject!: (reason?: unknown) => void
  const promise = new Promise<T>((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

async function flushMicrotasks(): Promise<void> {
  for (let index = 0; index < 8; index += 1) {
    await Promise.resolve()
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
    expect(
      useWorkflowGraphStore.getState().getSnapshot(7)?.graph_revision
    ).toBe(3)

    store.applyFromDetail(7, baseSnapshot({ graph_revision: 5 }))
    expect(
      useWorkflowGraphStore.getState().getSnapshot(7)?.graph_revision
    ).toBe(5)
  })

  it("discards fetched snapshot when request generation is stale", async () => {
    const resolvers: Array<(v: WorkflowGraphSnapshot | null) => void> = []
    getWorkflowGraphSnapshot.mockImplementation(
      () =>
        new Promise<WorkflowGraphSnapshot | null>((resolve) => {
          resolvers.push(resolve)
        })
    )

    const release = useWorkflowGraphStore.getState().activateConversation(11)
    useWorkflowGraphStore
      .getState()
      .applyFromDetail(11, baseSnapshot({ graph_revision: 1 }))

    // The automatic initial refresh is generation 1.
    await vi.waitFor(() => expect(resolvers.length).toBe(1))
    expect(
      useWorkflowGraphStore.getState().getEntry(11)?.requestGeneration
    ).toBe(1)

    // The compatibility nudge supersedes it with generation 2.
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

    release()
  })

  it("graph_changed with lower-or-equal revision does not refetch", async () => {
    getWorkflowGraphSnapshot
      .mockResolvedValueOnce(baseSnapshot({ graph_revision: 5 }))
      .mockResolvedValue(baseSnapshot({ graph_revision: 10 }))
    useWorkflowGraphStore
      .getState()
      .applyFromDetail(3, baseSnapshot({ graph_revision: 5 }))
    const release = useWorkflowGraphStore.getState().activateConversation(3)

    await vi.waitFor(() => {
      expect(
        useWorkflowGraphStore.getState().getSnapshot(3)?.graph_revision
      ).toBe(5)
    })
    useWorkflowGraphStore
      .getState()
      .applyFromDetail(3, baseSnapshot({ graph_revision: 5 }))
    expect(
      useWorkflowGraphStore.getState().getSnapshot(3)?.graph_revision
    ).toBe(5)
    getWorkflowGraphSnapshot.mockClear()

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
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
    release()
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
    const release = useWorkflowGraphStore.getState().activateConversation(9)
    await vi.waitFor(() => {
      expect(
        useWorkflowGraphStore.getState().getSnapshot(9)?.compatibility
      ).toBe("observed_only")
    })
    const generationAfterInitial =
      useWorkflowGraphStore.getState().getEntry(9)?.requestGeneration ?? 0
    getWorkflowGraphSnapshot.mockClear()

    useWorkflowGraphStore.getState().handleCompatibilityNudge({
      parent_conversation_id: 9,
    })
    await vi.waitFor(() => {
      expect(
        useWorkflowGraphStore.getState().getSnapshot(9)?.compatibility
      ).toBe("observed_only")
    })
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
    expect(
      useWorkflowGraphStore.getState().getEntry(9)?.requestGeneration
    ).toBe(generationAfterInitial + 1)
    release()
  })
})

describe("workflow activation lifecycle", () => {
  beforeEach(() => {
    vi.useFakeTimers()
  })

  afterEach(() => {
    vi.useRealTimers()
  })

  it("waits for both listener attempts before the first activation refresh", async () => {
    const changed = deferred<() => void>()
    const nudge = deferred<() => void>()
    subscribeWorkflowGraphChanged.mockReturnValue(changed.promise)
    subscribeWorkflowCompatibilityNudge.mockReturnValue(nudge.promise)
    getWorkflowGraphSnapshot.mockResolvedValue(
      baseSnapshot({ graph_revision: 2 })
    )

    const release = useWorkflowGraphStore.getState().activateConversation(41)
    changed.resolve(vi.fn())
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).not.toHaveBeenCalled()

    nudge.resolve(vi.fn())
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledWith(41)
    release()
  })

  it("keeps the pending initial refresh when the zero-to-one lease releases first", async () => {
    const changed = deferred<() => void>()
    const nudge = deferred<() => void>()
    subscribeWorkflowGraphChanged.mockReturnValue(changed.promise)
    subscribeWorkflowCompatibilityNudge.mockReturnValue(nudge.promise)
    getWorkflowGraphSnapshot.mockResolvedValue(
      baseSnapshot({ graph_revision: 2 })
    )

    const firstRelease = useWorkflowGraphStore
      .getState()
      .activateConversation(42)
    const secondRelease = useWorkflowGraphStore
      .getState()
      .activateConversation(42)
    firstRelease()

    changed.resolve(vi.fn())
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).not.toHaveBeenCalled()

    nudge.resolve(vi.fn())
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledWith(42)

    firstRelease()
    secondRelease()
  })

  it("keeps one listener install until two different conversations both release", async () => {
    const changedDispose = vi.fn()
    const nudgeDispose = vi.fn()
    subscribeWorkflowGraphChanged.mockResolvedValue(changedDispose)
    subscribeWorkflowCompatibilityNudge.mockResolvedValue(nudgeDispose)
    getWorkflowGraphSnapshot.mockResolvedValue(baseSnapshot())

    const firstRelease = useWorkflowGraphStore
      .getState()
      .activateConversation(51)
    const secondRelease = useWorkflowGraphStore
      .getState()
      .activateConversation(52)
    await flushMicrotasks()

    expect(subscribeWorkflowGraphChanged).toHaveBeenCalledTimes(1)
    expect(subscribeWorkflowCompatibilityNudge).toHaveBeenCalledTimes(1)

    firstRelease()
    expect(changedDispose).not.toHaveBeenCalled()
    expect(nudgeDispose).not.toHaveBeenCalled()

    secondRelease()
    expect(changedDispose).toHaveBeenCalledTimes(1)
    expect(nudgeDispose).toHaveBeenCalledTimes(1)
  })

  it("keeps a successful listener owned when its sibling rejects", async () => {
    const changed = deferred<() => void>()
    const nudge = deferred<() => void>()
    const nudgeDispose = vi.fn()
    subscribeWorkflowGraphChanged.mockReturnValue(changed.promise)
    subscribeWorkflowCompatibilityNudge.mockReturnValue(nudge.promise)
    getWorkflowGraphSnapshot.mockResolvedValue(baseSnapshot())

    const release = useWorkflowGraphStore.getState().activateConversation(53)
    changed.reject(new Error("changed unavailable"))
    await flushMicrotasks()
    vi.advanceTimersByTime(4_999)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).not.toHaveBeenCalled()

    nudge.resolve(nudgeDispose)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledWith(53)
    expect(nudgeDispose).not.toHaveBeenCalled()

    release()
    expect(nudgeDispose).toHaveBeenCalledTimes(1)
    release()
    expect(nudgeDispose).toHaveBeenCalledTimes(1)
  })

  it("starts refresh-only mode at the five-second readiness deadline", async () => {
    const changed = deferred<() => void>()
    const nudge = deferred<() => void>()
    const changedDispose = vi.fn()
    const lateNudgeDispose = vi.fn()
    subscribeWorkflowGraphChanged.mockReturnValue(changed.promise)
    subscribeWorkflowCompatibilityNudge.mockReturnValue(nudge.promise)
    getWorkflowGraphSnapshot.mockResolvedValue(baseSnapshot())

    const release = useWorkflowGraphStore.getState().activateConversation(54)
    changed.resolve(changedDispose)
    await flushMicrotasks()
    vi.advanceTimersByTime(4_999)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).not.toHaveBeenCalled()

    vi.advanceTimersByTime(1)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledWith(54)

    release()
    expect(changedDispose).toHaveBeenCalledTimes(1)
    nudge.resolve(lateNudgeDispose)
    await flushMicrotasks()
    expect(lateNudgeDispose).toHaveBeenCalledTimes(1)
  })

  it("deactivation before readiness prevents the initial refresh", async () => {
    const changed = deferred<() => void>()
    const nudge = deferred<() => void>()
    const changedDispose = vi.fn()
    const nudgeDispose = vi.fn()
    subscribeWorkflowGraphChanged.mockReturnValue(changed.promise)
    subscribeWorkflowCompatibilityNudge.mockReturnValue(nudge.promise)

    const release = useWorkflowGraphStore.getState().activateConversation(55)
    release()
    changed.resolve(changedDispose)
    nudge.resolve(nudgeDispose)
    await flushMicrotasks()
    vi.advanceTimersByTime(5_001)
    await flushMicrotasks()

    expect(getWorkflowGraphSnapshot).not.toHaveBeenCalled()
    expect(changedDispose).toHaveBeenCalledTimes(1)
    expect(nudgeDispose).toHaveBeenCalledTimes(1)
  })

  it("old readiness callback and deadline cannot fetch into a reactivated epoch", async () => {
    const changed1 = deferred<() => void>()
    const nudge1 = deferred<() => void>()
    const changed2 = deferred<() => void>()
    const nudge2 = deferred<() => void>()
    const changedDispose1 = vi.fn()
    const nudgeDispose1 = vi.fn()
    const changedDispose2 = vi.fn()
    const nudgeDispose2 = vi.fn()
    subscribeWorkflowGraphChanged
      .mockReturnValueOnce(changed1.promise)
      .mockReturnValueOnce(changed2.promise)
    subscribeWorkflowCompatibilityNudge
      .mockReturnValueOnce(nudge1.promise)
      .mockReturnValueOnce(nudge2.promise)
    getWorkflowGraphSnapshot.mockResolvedValue(baseSnapshot())

    const release1 = useWorkflowGraphStore.getState().activateConversation(56)
    vi.advanceTimersByTime(4_999)
    await flushMicrotasks()
    release1()
    const release2 = useWorkflowGraphStore.getState().activateConversation(56)

    vi.advanceTimersByTime(1)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).not.toHaveBeenCalled()

    changed1.resolve(changedDispose1)
    nudge1.resolve(nudgeDispose1)
    await flushMicrotasks()
    expect(changedDispose1).toHaveBeenCalledTimes(1)
    expect(nudgeDispose1).toHaveBeenCalledTimes(1)
    expect(getWorkflowGraphSnapshot).not.toHaveBeenCalled()

    changed2.resolve(changedDispose2)
    nudge2.resolve(nudgeDispose2)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledWith(56)

    release2()
    expect(changedDispose2).toHaveBeenCalledTimes(1)
    expect(nudgeDispose2).toHaveBeenCalledTimes(1)
  })

  it("a stale lease cannot release a later activation epoch", async () => {
    getWorkflowGraphSnapshot.mockResolvedValue(baseSnapshot())

    const staleRelease = useWorkflowGraphStore
      .getState()
      .activateConversation(57)
    useWorkflowGraphStore.getState().reset()
    const liveRelease = useWorkflowGraphStore
      .getState()
      .activateConversation(57)
    staleRelease()
    staleRelease()
    await flushMicrotasks()
    getWorkflowGraphSnapshot.mockClear()

    useWorkflowGraphStore.getState().handleCompatibilityNudge({
      parent_conversation_id: 57,
    })
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledWith(57)
    liveRelease()
  })

  it("ignores non-positive conversation ids", async () => {
    const releaseZero = useWorkflowGraphStore.getState().activateConversation(0)
    const releaseNegative = useWorkflowGraphStore
      .getState()
      .activateConversation(-1)
    await flushMicrotasks()

    expect(subscribeWorkflowGraphChanged).not.toHaveBeenCalled()
    expect(subscribeWorkflowCompatibilityNudge).not.toHaveBeenCalled()
    expect(getWorkflowGraphSnapshot).not.toHaveBeenCalled()
    releaseZero()
    releaseNegative()
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

describe("task position (implementer-only / distinct task_index)", () => {
  it("counts implementer nodes only — reviewers with task_index do not inflate total", () => {
    const nodes: WorkflowNodeSnapshot[] = [
      node({
        node_id: "t1-impl",
        phase_id: "tasks",
        role: "implementer",
        task_index: 1,
        status: "completed",
      }),
      node({
        node_id: "t1-rev",
        phase_id: "tasks",
        role: "reviewer",
        task_index: 1,
        status: "completed",
      }),
      node({
        node_id: "t2-impl",
        phase_id: "tasks",
        role: "implementer",
        task_index: 2,
        status: "running",
      }),
      node({
        node_id: "t2-rev",
        phase_id: "tasks",
        role: "reviewer",
        task_index: 2,
        status: "estimated",
      }),
      node({
        node_id: "t3-impl",
        phase_id: "tasks",
        role: "implementer",
        task_index: 3,
        status: "estimated",
      }),
      node({
        node_id: "t3-rev",
        phase_id: "tasks",
        role: "reviewer",
        task_index: 3,
        status: "estimated",
      }),
    ]
    // Bug regression: counting `task_index != null` would yield total=6.
    // Implementer-only total 3; current from incomplete pairs / current ids → 2.
    const progress = computeTaskPhaseProgress(nodes, ["t2-impl"])
    expect(progress).toEqual({ current: 2, total: 3 })

    const rail = buildPhaseRail(
      baseSnapshot({
        current_phase_id: "tasks",
        current_node_ids: ["t2-impl"],
        nodes,
        gates: [],
      })
    )
    expect(rail.find((p) => p.kind === "tasks")?.taskProgress).toEqual({
      current: 2,
      total: 3,
    })
  })

  it("uses distinct task_index for total when multiple implementers share indices", () => {
    const nodes: WorkflowNodeSnapshot[] = [
      node({
        node_id: "a",
        role: "implementer",
        task_index: 1,
        status: "completed",
      }),
      node({
        node_id: "b",
        role: "implementer",
        task_index: 2,
        status: "completed",
      }),
      node({
        node_id: "c",
        role: "implementer",
        task_index: 3,
        status: "estimated",
      }),
    ]
    expect(computeTaskPhaseProgress(nodes)).toEqual({
      current: 3,
      total: 3,
    })
  })

  it("does not jump ahead while reviewer is active on an earlier task", () => {
    const nodes: WorkflowNodeSnapshot[] = [
      node({
        node_id: "t1-impl",
        phase_id: "tasks",
        role: "implementer",
        task_index: 1,
        status: "completed",
      }),
      node({
        node_id: "t1-rev",
        phase_id: "tasks",
        role: "reviewer",
        task_index: 1,
        status: "running",
      }),
      node({
        node_id: "t2-impl",
        phase_id: "tasks",
        role: "implementer",
        task_index: 2,
        status: "running",
      }),
      node({
        node_id: "t2-rev",
        phase_id: "tasks",
        role: "reviewer",
        task_index: 2,
        status: "estimated",
      }),
      node({
        node_id: "t3-impl",
        phase_id: "tasks",
        role: "implementer",
        task_index: 3,
        status: "estimated",
      }),
    ]

    // current_node_ids include both the active earlier reviewer and a later
    // implementer — position must stay at min task_index (1).
    expect(computeTaskPhaseProgress(nodes, ["t1-rev", "t2-impl"])).toEqual({
      current: 1,
      total: 3,
    })

    // Even if only the later implementer is listed as current, incomplete
    // implementer/reviewer pair on task 1 keeps position at 1.
    expect(computeTaskPhaseProgress(nodes, ["t2-impl"])).toEqual({
      current: 1,
      total: 3,
    })

    // Old bug: first active implementer alone would report Task 2 / 3.
    const rail = buildPhaseRail(
      baseSnapshot({
        current_phase_id: "tasks",
        current_node_ids: ["t2-impl"],
        nodes,
        gates: [],
      })
    )
    expect(rail.find((p) => p.kind === "tasks")?.taskProgress).toEqual({
      current: 1,
      total: 3,
    })
  })

  it("uses min task_index from current_node_ids including active reviewers", () => {
    const nodes: WorkflowNodeSnapshot[] = [
      node({
        node_id: "t1-impl",
        role: "implementer",
        task_index: 1,
        status: "completed",
      }),
      node({
        node_id: "t1-rev",
        role: "reviewer",
        task_index: 1,
        status: "waiting_review",
      }),
      node({
        node_id: "t2-impl",
        role: "implementer",
        task_index: 2,
        status: "estimated",
      }),
    ]
    expect(computeTaskPhaseProgress(nodes, ["t1-rev"])).toEqual({
      current: 1,
      total: 2,
    })
  })
})

describe("deterministic workflow lane rows", () => {
  it("keeps one Task row for an implementer and one reviewer", () => {
    const rail = buildPhaseRail(
      baseSnapshot({
        current_phase_id: "tasks",
        current_node_ids: ["task-1-review"],
        nodes: [
          node({
            node_id: "task-1-impl",
            phase_id: "tasks",
            role: "implementer",
            task_index: 1,
            status: "completed",
            task_risk_level: "normal",
            required_reviewer_count: 1,
            returned_reviewer_count: 0,
          }),
          node({
            node_id: "task-1-review",
            phase_id: "tasks",
            role: "reviewer",
            task_index: 1,
            status: "running",
            task_risk_level: "normal",
            required_reviewer_count: 1,
            returned_reviewer_count: 0,
          }),
        ],
        gates: [],
      })
    )

    const tasks = rail.find((phase) => phase.kind === "tasks")
    expect(tasks?.nodeRows).toHaveLength(1)
    expect(tasks?.nodeRows[0].nodes.map((item) => item.node_id)).toEqual([
      "task-1-impl",
      "task-1-review",
    ])
    expect(tasks?.nodeRows[0].reviewerProgress).toEqual({
      returned: 0,
      required: 1,
    })
  })

  it("preserves policy order for a two-reviewer Task cohort", () => {
    const nodes = [
      node({
        node_id: "task-2-grok",
        phase_id: "tasks",
        role: "reviewer",
        task_index: 2,
        status: "completed",
        task_risk_level: "high",
        required_reviewer_count: 2,
        returned_reviewer_count: 1,
      }),
      node({
        node_id: "task-2-impl",
        phase_id: "tasks",
        role: "implementer",
        task_index: 2,
        status: "completed",
        task_risk_level: "high",
        required_reviewer_count: 2,
        returned_reviewer_count: 1,
      }),
      node({
        node_id: "task-2-codex",
        phase_id: "tasks",
        role: "reviewer",
        task_index: 2,
        status: "running",
        task_risk_level: "high",
        required_reviewer_count: 2,
        returned_reviewer_count: 1,
      }),
    ]
    const tasks = buildPhaseRail(
      baseSnapshot({
        current_phase_id: "tasks",
        current_node_ids: ["task-2-codex"],
        nodes,
        gates: [],
      })
    ).find((phase) => phase.kind === "tasks")

    expect(tasks?.taskProgress).toEqual({ current: 1, total: 1 })
    expect(tasks?.nodeRows).toHaveLength(1)
    expect(tasks?.nodeRows[0].nodes.map((item) => item.node_id)).toEqual([
      "task-2-impl",
      "task-2-grok",
      "task-2-codex",
    ])
    expect(tasks?.nodeRows[0].reviewerProgress).toEqual({
      returned: 1,
      required: 2,
    })
  })

  it("holds progress on an earlier Task until both reviewers return", () => {
    const nodes = [
      node({
        node_id: "task-1-impl",
        phase_id: "tasks",
        role: "implementer",
        task_index: 1,
        status: "completed",
      }),
      node({
        node_id: "task-1-review-a",
        phase_id: "tasks",
        role: "reviewer",
        task_index: 1,
        status: "completed",
      }),
      node({
        node_id: "task-1-review-b",
        phase_id: "tasks",
        role: "reviewer",
        task_index: 1,
        status: "running",
      }),
      node({
        node_id: "task-2-impl",
        phase_id: "tasks",
        role: "implementer",
        task_index: 2,
        status: "running",
      }),
    ]

    expect(computeTaskPhaseProgress(nodes, ["task-2-impl"])).toEqual({
      current: 1,
      total: 2,
    })
  })

  it("places the Plan Author before its reviewer cohort", () => {
    const plan = buildPhaseRail(
      baseSnapshot({
        nodes: [
          node({
            node_id: "plan-review-grok",
            phase_id: "plan",
            role: "reviewer",
          }),
          node({
            node_id: "plan-author",
            phase_id: "plan",
            role: "author",
          }),
          node({
            node_id: "plan-review-codex",
            phase_id: "plan",
            role: "reviewer",
          }),
        ],
      })
    ).find((phase) => phase.kind === "plan")

    expect(plan?.nodeRows).toHaveLength(1)
    expect(plan?.nodeRows[0].nodes.map((item) => item.node_id)).toEqual([
      "plan-author",
      "plan-review-grok",
      "plan-review-codex",
    ])
  })

  it("does not invent reviewer counts for older observed-only snapshots", () => {
    const tasks = buildPhaseRail(
      baseSnapshot({
        schema_version: 1,
        compatibility: "observed_only",
        nodes: [
          node({
            node_id: "observed-impl",
            phase_id: "tasks",
            role: "implementer",
            task_index: 1,
            status: "completed",
            is_observed: true,
          }),
          node({
            node_id: "observed-review",
            phase_id: "tasks",
            role: "reviewer",
            task_index: 1,
            status: "completed",
            is_observed: true,
          }),
        ],
        gates: [],
      })
    ).find((phase) => phase.kind === "tasks")

    expect(tasks?.nodeRows).toHaveLength(1)
    expect(tasks?.nodeRows[0].reviewerProgress).toBeNull()
  })
})

describe("event subscription Strict Mode / generation token", () => {
  it("stale install dispose does not overwrite the live unsub handles", async () => {
    type Dispose = () => void
    const changedResolvers: Array<(d: Dispose) => void> = []
    const nudgeResolvers: Array<(d: Dispose) => void> = []
    const disposedHandles: Dispose[] = []

    subscribeWorkflowGraphChanged.mockImplementation(
      async () =>
        await new Promise<Dispose>((resolve) => {
          changedResolvers.push(resolve)
        })
    )
    subscribeWorkflowCompatibilityNudge.mockImplementation(
      async () =>
        await new Promise<Dispose>((resolve) => {
          nudgeResolvers.push(resolve)
        })
    )

    // Install 1 (first mount).
    const unmount1 = useWorkflowGraphStore.getState().activateConversation(50)
    await vi.waitFor(() => expect(changedResolvers.length).toBe(1))
    expect(__getWorkflowGraphEventInstallGenerationForTests()).toBeGreaterThan(
      0
    )

    // Strict Mode unmount before subscribe promises settle.
    unmount1()
    expect(__getWorkflowGraphEventInstallGenerationForTests()).toBe(0)

    // Remount → install 2.
    const unmount2 = useWorkflowGraphStore.getState().activateConversation(50)
    await vi.waitFor(() => expect(changedResolvers.length).toBe(2))
    const liveGen = __getWorkflowGraphEventInstallGenerationForTests()
    expect(liveGen).toBeGreaterThan(0)

    const staleChanged = vi.fn(() => {
      disposedHandles.push(staleChanged)
    })
    const liveChanged = vi.fn(() => {
      disposedHandles.push(liveChanged)
    })
    const staleNudge = vi.fn()
    const liveNudge = vi.fn()

    // Stale install-1 resolves after remount — must self-dispose, not assign.
    changedResolvers[0](staleChanged)
    nudgeResolvers[0](staleNudge)
    await Promise.resolve()
    await Promise.resolve()
    expect(staleChanged).toHaveBeenCalledTimes(1)
    expect(staleNudge).toHaveBeenCalledTimes(1)
    // Live install still active.
    expect(__getWorkflowGraphEventInstallGenerationForTests()).toBe(liveGen)

    // Live install-2 resolves and owns the slots.
    changedResolvers[1](liveChanged)
    nudgeResolvers[1](liveNudge)
    await Promise.resolve()
    await Promise.resolve()
    expect(liveChanged).not.toHaveBeenCalled()

    // Final unmount disposes only the live handles.
    unmount2()
    expect(liveChanged).toHaveBeenCalledTimes(1)
    expect(liveNudge).toHaveBeenCalledTimes(1)
    expect(__getWorkflowGraphEventInstallGenerationForTests()).toBe(0)
  })

  it("eventInstallGeneration is monotonic — reset only clears active reference", () => {
    const counterBefore = __getWorkflowGraphEventInstallCounterForTests()

    const un1 = useWorkflowGraphStore.getState().activateConversation(61)
    const afterFirst = __getWorkflowGraphEventInstallCounterForTests()
    expect(afterFirst).toBe(counterBefore + 1)
    expect(__getWorkflowGraphEventInstallGenerationForTests()).toBe(afterFirst)

    un1()
    // Active cleared; counter must not decrease or zero.
    expect(__getWorkflowGraphEventInstallGenerationForTests()).toBe(0)
    expect(__getWorkflowGraphEventInstallCounterForTests()).toBe(afterFirst)

    const un2 = useWorkflowGraphStore.getState().activateConversation(61)
    const afterSecond = __getWorkflowGraphEventInstallCounterForTests()
    expect(afterSecond).toBe(afterFirst + 1)
    expect(__getWorkflowGraphEventInstallGenerationForTests()).toBe(afterSecond)

    un2()
    useWorkflowGraphStore.getState().reset()
    // reset must not rewind the monotonic counter.
    expect(__getWorkflowGraphEventInstallCounterForTests()).toBe(afterSecond)
    expect(__getWorkflowGraphEventInstallGenerationForTests()).toBe(0)

    const un3 = useWorkflowGraphStore.getState().activateConversation(62)
    expect(__getWorkflowGraphEventInstallCounterForTests()).toBe(
      afterSecond + 1
    )
    un3()
  })
})
