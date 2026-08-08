import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

import type {
  DelegationRuntimeStats,
  WorkflowGraphSnapshot,
  WorkflowNodeSnapshot,
} from "@/lib/types"

const {
  WORKFLOW_GRAPH_CHANGED_EVENT,
  WORKFLOW_GRAPH_COMPATIBILITY_NUDGE_EVENT,
  getWorkflowGraphSnapshot,
  subscribeCompletionDecisionResolved,
  subscribeWorkflowGraphChanged,
  subscribeWorkflowCompatibilityNudge,
} = vi.hoisted(() => ({
  WORKFLOW_GRAPH_CHANGED_EVENT: "workflow_graph://changed",
  WORKFLOW_GRAPH_COMPATIBILITY_NUDGE_EVENT:
    "workflow_graph://compatibility_nudge",
  getWorkflowGraphSnapshot: vi.fn(),
  subscribeCompletionDecisionResolved: vi.fn(async () => () => {}),
  subscribeWorkflowGraphChanged: vi.fn(async () => () => {}),
  subscribeWorkflowCompatibilityNudge: vi.fn(async () => () => {}),
}))

// Pass hoisted mocks through directly — do not re-wrap with `...args: unknown[]`
// spreads (TS2556: spread of unknown[] is not a rest tuple).
vi.mock("@/lib/api", () => ({
  WORKFLOW_GRAPH_CHANGED_EVENT,
  WORKFLOW_GRAPH_COMPATIBILITY_NUDGE_EVENT,
  getWorkflowGraphSnapshot,
  subscribeCompletionDecisionResolved,
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
    model: null,
    effort: null,
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
    is_observed: false,
    retained_observed: false,
    required: true,
    node_outcome: null,
    deps: [],
    ...overrides,
  }
}

function activeSnapshot(
  overrides: Partial<WorkflowGraphSnapshot> = {}
): WorkflowGraphSnapshot {
  return baseSnapshot({
    overall_state: "in_progress",
    current_phase_id: "tasks",
    current_node_ids: ["n-task-active"],
    nodes: [
      node({
        node_id: "n-task-active",
        phase_id: "tasks",
        role: "implementer",
        status: "running",
        is_observed: true,
        latest_child_conversation_id: 42,
      }),
    ],
    gates: [],
    ...overrides,
  })
}

function settledSnapshot(
  overrides: Partial<WorkflowGraphSnapshot> = {}
): WorkflowGraphSnapshot {
  return baseSnapshot({
    overall_state: "completed",
    current_phase_id: "final",
    current_node_ids: [],
    nodes: [
      node({
        node_id: "n-final-complete",
        phase_id: "final",
        role: "reviewer",
        status: "completed",
        is_observed: true,
        latest_child_conversation_id: 42,
      }),
    ],
    gates: [],
    ...overrides,
  })
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
  for (let index = 0; index < 16; index += 1) {
    await Promise.resolve()
  }
}

beforeEach(() => {
  __resetWorkflowGraphStoreForTests()
  getWorkflowGraphSnapshot.mockReset()
  subscribeWorkflowGraphChanged.mockReset()
  subscribeCompletionDecisionResolved.mockReset()
  subscribeWorkflowCompatibilityNudge.mockReset()
  subscribeWorkflowGraphChanged.mockResolvedValue(() => {})
  subscribeCompletionDecisionResolved.mockResolvedValue(() => {})
  subscribeWorkflowCompatibilityNudge.mockResolvedValue(() => {})
})

afterEach(() => {
  __resetWorkflowGraphStoreForTests()
})

describe("workflow-graph-store revision gate", () => {
  it("uses completion events only to refetch an authoritative durable snapshot", async () => {
    const needsDecision = baseSnapshot({
      graph_revision: 7,
      nodes: [
        node({
          node_id: "n-plan-r1",
          latest_task_id: "task-1",
          completion: {
            protocol_version: 2,
            graph_revision: 7,
            card: {
              state: "needs_decision",
              role: "reviewer",
              outcome: null,
              summary: "Choose an outcome.",
              report_file: null,
              source: null,
              evidence_validated: false,
              attention: {
                attention_id: "attention-1",
                task_id: "task-1",
                kind: "completion_decision",
                captured_scope_digest: `sha256:${"a".repeat(64)}`,
                latest_run_id: "task-1",
                node_id: "n-plan-r1",
              },
            },
          },
        }),
      ],
    })
    const event = {
      version: 1,
      event_id: "event-1",
      workflow_id: "wf-1",
      task_id: "task-1",
      node_id: "n-plan-r1",
      kind: "completion_decision" as const,
      outcome: "approve_with_minors" as const,
      evidence_scope_digest: `sha256:${"a".repeat(64)}`,
      graph_revision: 8,
    }
    const artifactRecovery = baseSnapshot({
      graph_revision: 8,
      nodes: [
        node({
          node_id: "n-plan-r1",
          latest_task_id: "task-1",
          completion: {
            protocol_version: 2,
            graph_revision: 8,
            card: {
              state: "blocked",
              role: "reviewer",
              outcome: "approve_with_minors",
              summary: "Artifact recovery is required.",
              report_file: null,
              source: "user_adjudication",
              evidence_validated: false,
              attention: {
                ...needsDecision.nodes[0].completion!.card.attention!,
                attention_id: "attention-recovery",
                kind: "completion_artifact_recovery",
              },
            },
          },
        }),
      ],
    })

    useWorkflowGraphStore.getState().applyFromDetail(7, needsDecision)
    getWorkflowGraphSnapshot.mockResolvedValueOnce(needsDecision)
    const release = useWorkflowGraphStore.getState().activateConversation(7)
    await flushMicrotasks()
    getWorkflowGraphSnapshot.mockClear()
    getWorkflowGraphSnapshot.mockResolvedValue(artifactRecovery)

    useWorkflowGraphStore.getState().handleCompletionDecisionResolved(event)
    useWorkflowGraphStore.getState().handleCompletionDecisionResolved(event)

    expect(
      useWorkflowGraphStore.getState().getSnapshot(7)?.nodes[0].completion?.card
        .state
    ).toBe("needs_decision")
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
    expect(
      useWorkflowGraphStore.getState().getSnapshot(7)?.nodes[0].completion?.card
        .state
    ).toBe("blocked")
    expect(
      useWorkflowGraphStore.getState().getSnapshot(7)?.nodes[0].completion?.card
        .attention?.kind
    ).toBe("completion_artifact_recovery")

    useWorkflowGraphStore.getState().handleCompletionDecisionResolved({
      ...event,
      event_id: "event-scope-mismatch",
      evidence_scope_digest: `sha256:${"c".repeat(64)}`,
      graph_revision: 9,
    })
    useWorkflowGraphStore.getState().handleCompletionDecisionResolved({
      ...event,
      event_id: "event-delayed-task",
      task_id: "task-previous",
      graph_revision: 9,
    })
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
    release()
  })

  it("refetches completion events when raw node ids differ from public graph ids", async () => {
    const publicNodeId = "pub_8d705a27d44d891f"
    const rawNodeId = "reviewers/plan/codex"
    const attention = {
      attention_id: "attention-mapped",
      task_id: "task-mapped",
      kind: "completion_decision" as const,
      captured_scope_digest: `sha256:${"b".repeat(64)}`,
      latest_run_id: "task-mapped",
      node_id: publicNodeId,
    }
    const pending = baseSnapshot({
      graph_revision: 7,
      completion: {
        protocol_version: 2,
        graph_revision: 7,
        card: {
          state: "needs_decision",
          role: "reviewer",
          outcome: null,
          summary: "Another decision is selected for the graph summary.",
          report_file: null,
          source: null,
          evidence_validated: false,
          attention: {
            ...attention,
            attention_id: "attention-other",
            task_id: "task-other",
            latest_run_id: "task-other",
            node_id: "plan-reviewer-other",
          },
        },
      },
      nodes: [
        node({
          node_id: publicNodeId,
          latest_task_id: "task-mapped",
          completion: {
            protocol_version: 2,
            graph_revision: 7,
            card: {
              state: "needs_decision",
              role: "reviewer",
              outcome: null,
              summary: "Choose an outcome.",
              report_file: null,
              source: null,
              evidence_validated: false,
              attention,
            },
          },
        }),
      ],
    })

    useWorkflowGraphStore.getState().applyFromDetail(17, pending)
    getWorkflowGraphSnapshot.mockResolvedValueOnce(pending)
    const release = useWorkflowGraphStore.getState().activateConversation(17)
    await flushMicrotasks()
    getWorkflowGraphSnapshot.mockClear()
    getWorkflowGraphSnapshot.mockResolvedValue(
      baseSnapshot({ graph_revision: 8, completion: null, nodes: [] })
    )

    useWorkflowGraphStore.getState().handleCompletionDecisionResolved({
      version: 1,
      event_id: "event-mapped",
      workflow_id: "wf-1",
      task_id: "task-mapped",
      node_id: rawNodeId,
      kind: "completion_decision",
      outcome: "approve",
      evidence_scope_digest: attention.captured_scope_digest,
      graph_revision: 8,
    })
    await flushMicrotasks()

    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledWith(17)
    release()
  })

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

  it("replaces runtime fields only on nodes for the exact latest task", () => {
    const snapshot = baseSnapshot({
      graph_revision: 12,
      nodes: [
        node({ node_id: "current", latest_task_id: "task-current" }),
        node({ node_id: "stale", latest_task_id: "task-newer" }),
      ],
    })
    useWorkflowGraphStore.getState().applyFromDetail(120, snapshot)
    const before = useWorkflowGraphStore.getState().getEntry(120)
    const stats: DelegationRuntimeStats = {
      started_at: "2026-08-03T00:00:00.000Z",
      finished_at: null,
      tool_call_count: 9,
      edit_tool_call_count: 4,
      touched_files: [
        {
          path: "src/a.ts",
          outside_workspace: false,
          additions: 7,
          deletions: 2,
        },
        { path: "src/b.ts", outside_workspace: false },
      ],
      touched_files_truncated: true,
      additions: 7,
      deletions: 2,
      line_counts_complete: true,
    }

    useWorkflowGraphStore.getState().applyRuntimeStats("task-current", stats)

    const after = useWorkflowGraphStore.getState().getEntry(120)
    const current = after?.snapshot?.nodes.find(
      (item) => item.node_id === "current"
    )
    const stale = after?.snapshot?.nodes.find(
      (item) => item.node_id === "stale"
    )
    expect(current).toMatchObject({
      tool_call_count: 9,
      edit_tool_call_count: 4,
      touched_file_count: 2,
      touched_files_truncated: true,
      additions: 7,
      deletions: 2,
      line_counts_complete: true,
    })
    expect(stale?.tool_call_count).toBeNull()
    expect(after?.snapshot?.graph_revision).toBe(12)
    expect(after?.requestGeneration).toBe(before?.requestGeneration)

    const retained = after?.snapshot
    useWorkflowGraphStore
      .getState()
      .applyRuntimeStats("task-old", { ...stats, tool_call_count: 99 })
    expect(useWorkflowGraphStore.getState().getSnapshot(120)).toBe(retained)
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

  it("reconciles overlay interest after readiness even with a numbered cache", async () => {
    const changed = deferred<() => void>()
    const nudge = deferred<() => void>()
    subscribeWorkflowGraphChanged.mockReturnValue(changed.promise)
    subscribeWorkflowCompatibilityNudge.mockReturnValue(nudge.promise)
    getWorkflowGraphSnapshot.mockResolvedValue(
      baseSnapshot({ graph_revision: 4 })
    )

    const release = useWorkflowGraphStore.getState().activateOverlayInterest(90)
    changed.resolve(vi.fn())
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).not.toHaveBeenCalled()
    nudge.resolve(vi.fn())
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledWith(90)
    release()

    useWorkflowGraphStore
      .getState()
      .applyFromDetail(91, baseSnapshot({ graph_revision: 7 }))
    getWorkflowGraphSnapshot.mockResolvedValue(
      baseSnapshot({ graph_revision: 7 })
    )

    const firstRelease = useWorkflowGraphStore
      .getState()
      .activateOverlayInterest(91)
    await flushMicrotasks()
    firstRelease()

    getWorkflowGraphSnapshot.mockClear()
    getWorkflowGraphSnapshot.mockResolvedValue(
      baseSnapshot({ graph_revision: 8 })
    )
    const reactivatedRelease = useWorkflowGraphStore
      .getState()
      .activateOverlayInterest(91)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledWith(91)
    expect(
      useWorkflowGraphStore.getState().getSnapshot(91)?.graph_revision
    ).toBe(8)
    reactivatedRelease()
  })

  it("refreshes overlay interest when only an unnumbered snapshot is cached", async () => {
    useWorkflowGraphStore.getState().applyFromDetail(
      95,
      baseSnapshot({
        graph_revision: null,
        compatibility: "observed_only",
        overall_state: "observed_only",
        workflow_id: null,
      })
    )
    getWorkflowGraphSnapshot.mockResolvedValue(null)

    const release = useWorkflowGraphStore.getState().activateOverlayInterest(95)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledWith(95)
    release()
  })

  it("duplicate overlay leases share one pending first seed", async () => {
    const changed = deferred<() => void>()
    const nudge = deferred<() => void>()
    subscribeWorkflowGraphChanged.mockReturnValue(changed.promise)
    subscribeWorkflowCompatibilityNudge.mockReturnValue(nudge.promise)
    getWorkflowGraphSnapshot.mockResolvedValue(baseSnapshot())

    const creatorRelease = useWorkflowGraphStore
      .getState()
      .activateOverlayInterest(96)
    const remainingRelease = useWorkflowGraphStore
      .getState()
      .activateOverlayInterest(96)
    creatorRelease()
    changed.resolve(vi.fn())
    nudge.resolve(vi.fn())
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
    remainingRelease()
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

  it("warns once per required channel and shares one five-second retry timer", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {})
    subscribeWorkflowGraphChanged.mockRejectedValue(
      new Error("changed unavailable")
    )
    subscribeWorkflowCompatibilityNudge.mockRejectedValue(
      new Error("nudge unavailable")
    )
    getWorkflowGraphSnapshot.mockResolvedValue(
      settledSnapshot({ graph_revision: 2 })
    )

    const release = useWorkflowGraphStore
      .getState()
      .activateOverlayInterest(301)
    try {
      await flushMicrotasks()
      expect(subscribeWorkflowGraphChanged).toHaveBeenCalledTimes(1)
      expect(subscribeWorkflowCompatibilityNudge).toHaveBeenCalledTimes(1)
      expect(warn).toHaveBeenCalledWith(
        "[workflow-graph-store] required event subscription failed",
        {
          channel: WORKFLOW_GRAPH_CHANGED_EVENT,
          error: "changed unavailable",
        }
      )
      expect(warn).toHaveBeenCalledWith(
        "[workflow-graph-store] required event subscription failed",
        {
          channel: WORKFLOW_GRAPH_COMPATIBILITY_NUDGE_EVENT,
          error: "nudge unavailable",
        }
      )
      expect(warn).toHaveBeenCalledTimes(2)
      expect(vi.getTimerCount()).toBe(1)

      await vi.advanceTimersByTimeAsync(4_999)
      expect(subscribeWorkflowGraphChanged).toHaveBeenCalledTimes(1)
      expect(subscribeWorkflowCompatibilityNudge).toHaveBeenCalledTimes(1)
      await vi.advanceTimersByTimeAsync(1)
      await flushMicrotasks()

      expect(subscribeWorkflowGraphChanged).toHaveBeenCalledTimes(2)
      expect(subscribeWorkflowCompatibilityNudge).toHaveBeenCalledTimes(2)
      expect(warn).toHaveBeenCalledTimes(2)
      expect(vi.getTimerCount()).toBe(1)
    } finally {
      release()
      warn.mockRestore()
    }
  })

  it("retries only the missing required listener and retains its sibling", async () => {
    const changedDispose = vi.fn()
    const nudgeDispose = vi.fn()
    subscribeWorkflowGraphChanged
      .mockRejectedValueOnce(new Error("changed unavailable"))
      .mockResolvedValueOnce(changedDispose)
    subscribeWorkflowCompatibilityNudge.mockResolvedValue(nudgeDispose)
    getWorkflowGraphSnapshot.mockResolvedValue(
      settledSnapshot({ graph_revision: 2 })
    )

    const release = useWorkflowGraphStore
      .getState()
      .activateOverlayInterest(302)
    await flushMicrotasks()
    expect(subscribeWorkflowGraphChanged).toHaveBeenCalledTimes(1)
    expect(subscribeWorkflowCompatibilityNudge).toHaveBeenCalledTimes(1)

    await vi.advanceTimersByTimeAsync(5_000)
    await flushMicrotasks()
    expect(subscribeWorkflowGraphChanged).toHaveBeenCalledTimes(2)
    expect(subscribeWorkflowCompatibilityNudge).toHaveBeenCalledTimes(1)
    expect(changedDispose).not.toHaveBeenCalled()
    expect(nudgeDispose).not.toHaveBeenCalled()

    release()
    expect(changedDispose).toHaveBeenCalledTimes(1)
    expect(nudgeDispose).toHaveBeenCalledTimes(1)
  })

  it("final lease release clears retry, refresh, and warning generation state", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {})
    subscribeWorkflowGraphChanged.mockRejectedValue(
      new Error("changed unavailable")
    )
    subscribeWorkflowCompatibilityNudge.mockRejectedValue(
      new Error("nudge unavailable")
    )
    getWorkflowGraphSnapshot.mockResolvedValue(
      activeSnapshot({ graph_revision: 2 })
    )

    const firstRelease = useWorkflowGraphStore
      .getState()
      .activateOverlayInterest(303)
    try {
      await flushMicrotasks()
      expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
      expect(vi.getTimerCount()).toBe(2)

      firstRelease()
      expect(vi.getTimerCount()).toBe(0)
      await vi.advanceTimersByTimeAsync(15_000)
      await flushMicrotasks()
      expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
      expect(subscribeWorkflowGraphChanged).toHaveBeenCalledTimes(1)
      expect(subscribeWorkflowCompatibilityNudge).toHaveBeenCalledTimes(1)

      const secondRelease = useWorkflowGraphStore
        .getState()
        .activateOverlayInterest(303)
      await flushMicrotasks()
      expect(subscribeWorkflowGraphChanged).toHaveBeenCalledTimes(2)
      expect(subscribeWorkflowCompatibilityNudge).toHaveBeenCalledTimes(2)
      expect(warn).toHaveBeenCalledTimes(4)
      secondRelease()
      expect(vi.getTimerCount()).toBe(0)
    } finally {
      warn.mockRestore()
    }
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

describe("active workflow refresh scheduling", () => {
  beforeEach(() => {
    vi.useFakeTimers()
  })

  afterEach(() => {
    vi.useRealTimers()
  })

  it("active numbered overlay converges after 15 seconds and stops when settled", async () => {
    const active = activeSnapshot({ graph_revision: 2 })
    const settled = settledSnapshot({ graph_revision: 3 })
    useWorkflowGraphStore.getState().applyFromDetail(201, active)
    getWorkflowGraphSnapshot
      .mockResolvedValueOnce(active)
      .mockResolvedValueOnce(settled)

    const release = useWorkflowGraphStore
      .getState()
      .activateOverlayInterest(201)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)

    await vi.advanceTimersByTimeAsync(14_999)
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(1)
    await flushMicrotasks()

    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)
    expect(
      useWorkflowGraphStore.getState().getSnapshot(201)?.graph_revision
    ).toBe(3)
    expect(
      useWorkflowGraphStore.getState().getSnapshot(201)?.overall_state
    ).toBe("completed")

    await vi.advanceTimersByTimeAsync(20 * 60 * 1_000)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)
    release()
  })

  it.each([
    [
      "reserving node",
      settledSnapshot({
        graph_revision: 4,
        overall_state: "approved",
        nodes: [node({ node_id: "active", status: "reserving" })],
      }),
    ],
    [
      "running node",
      settledSnapshot({
        graph_revision: 4,
        overall_state: "approved",
        nodes: [node({ node_id: "active", status: "running" })],
      }),
    ],
    [
      "waiting_review node",
      settledSnapshot({
        graph_revision: 4,
        overall_state: "approved",
        nodes: [node({ node_id: "active", status: "waiting_review" })],
      }),
    ],
    [
      "waiting_adjudication node",
      settledSnapshot({
        graph_revision: 4,
        overall_state: "approved",
        nodes: [node({ node_id: "active", status: "waiting_adjudication" })],
      }),
    ],
    [
      "in_progress graph without active rows",
      settledSnapshot({
        graph_revision: 4,
        overall_state: "in_progress",
        nodes: [],
      }),
    ],
  ])("uses the 15-second authority timer for %s", async (_label, snapshot) => {
    getWorkflowGraphSnapshot.mockResolvedValue(snapshot)

    const release = useWorkflowGraphStore
      .getState()
      .activateOverlayInterest(202)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)

    await vi.advanceTimersByTimeAsync(14_999)
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(1)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)
    release()
  })

  it("settled numbered overlay handles newer events without a timer", async () => {
    useWorkflowGraphStore
      .getState()
      .applyFromDetail(92, settledSnapshot({ graph_revision: 2 }))
    getWorkflowGraphSnapshot.mockResolvedValue(
      settledSnapshot({ graph_revision: 3 })
    )
    const release = useWorkflowGraphStore.getState().activateOverlayInterest(92)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)

    useWorkflowGraphStore.getState().handleGraphChanged({
      parent_conversation_id: 92,
      workflow_id: "wf-1",
      graph_revision: 3,
    })
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)

    await vi.advanceTimersByTimeAsync(20 * 60 * 1_000)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
    release()
  })

  it("overlay-only discovery re-arms the 10-minute fallback until a graph appears", async () => {
    // b2d / writing-plans handoff: overlay opens on sessions while the
    // workflow is not yet published. If the first-publish event is missed,
    // the 10-minute safety net must still discover the graph so the chip
    // can leave the sub-agent-only state.
    getWorkflowGraphSnapshot
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce(settledSnapshot({ graph_revision: 1 }))

    const release = useWorkflowGraphStore.getState().activateOverlayInterest(94)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
    expect(useWorkflowGraphStore.getState().getSnapshot(94)).toBeNull()

    await vi.advanceTimersByTimeAsync(10 * 60 * 1_000)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)
    expect(useWorkflowGraphStore.getState().getSnapshot(94)).toBeNull()

    await vi.advanceTimersByTimeAsync(10 * 60 * 1_000)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(3)
    expect(
      useWorkflowGraphStore.getState().getSnapshot(94)?.graph_revision
    ).toBe(1)

    // Discovered graph under overlay-only: no further fallback polls.
    await vi.advanceTimersByTimeAsync(20 * 60 * 1_000)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(3)
    release()
  })

  it("seeds the first publish event after an overlay mount resolves null", async () => {
    const changedDispose = vi.fn()
    const nudgeDispose = vi.fn()
    subscribeWorkflowGraphChanged.mockResolvedValue(changedDispose)
    subscribeWorkflowCompatibilityNudge.mockResolvedValue(nudgeDispose)
    getWorkflowGraphSnapshot
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce(settledSnapshot({ graph_revision: 1 }))

    const release = useWorkflowGraphStore.getState().activateOverlayInterest(98)
    await flushMicrotasks()

    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
    expect(getWorkflowGraphSnapshot).toHaveBeenNthCalledWith(1, 98)
    expect(useWorkflowGraphStore.getState().getSnapshot(98)).toBeNull()

    useWorkflowGraphStore.getState().handleGraphChanged({
      parent_conversation_id: 98,
      workflow_id: "wf-1",
      graph_revision: 1,
    })
    await flushMicrotasks()

    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)
    expect(getWorkflowGraphSnapshot).toHaveBeenNthCalledWith(2, 98)
    expect(
      useWorkflowGraphStore.getState().getSnapshot(98)?.graph_revision
    ).toBe(1)

    // Event discovery disarms the overlay-only fallback once a revision lands.
    await vi.advanceTimersByTimeAsync(10 * 60 * 1_000)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)

    release()
    expect(changedDispose).toHaveBeenCalledTimes(1)
    expect(nudgeDispose).toHaveBeenCalledTimes(1)
    useWorkflowGraphStore.getState().handleGraphChanged({
      parent_conversation_id: 98,
      workflow_id: "wf-1",
      graph_revision: 2,
    })
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)
  })

  it("releasing expanded interest keeps overlay events but stops fallback", async () => {
    useWorkflowGraphStore
      .getState()
      .applyFromDetail(93, settledSnapshot({ graph_revision: 5 }))
    getWorkflowGraphSnapshot.mockResolvedValue(
      settledSnapshot({ graph_revision: 6 })
    )

    const releaseOverlay = useWorkflowGraphStore
      .getState()
      .activateOverlayInterest(93)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)

    const releaseExpanded = useWorkflowGraphStore
      .getState()
      .activateConversation(93)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)
    releaseExpanded()

    await vi.advanceTimersByTimeAsync(10 * 60 * 1_000)
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)
    useWorkflowGraphStore.getState().handleCompatibilityNudge({
      parent_conversation_id: 93,
    })
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(3)
    await vi.advanceTimersByTimeAsync(10 * 60 * 1_000)
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(3)

    releaseOverlay()
    useWorkflowGraphStore.getState().handleCompatibilityNudge({
      parent_conversation_id: 93,
    })
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(3)
  })

  it("late expanded completion updates cache but cannot arm an overlay timer", async () => {
    useWorkflowGraphStore
      .getState()
      .applyFromDetail(97, settledSnapshot({ graph_revision: 1 }))
    const pending = deferred<WorkflowGraphSnapshot | null>()
    getWorkflowGraphSnapshot.mockReturnValue(pending.promise)
    const releaseOverlay = useWorkflowGraphStore
      .getState()
      .activateOverlayInterest(97)
    await flushMicrotasks()
    const releaseExpanded = useWorkflowGraphStore
      .getState()
      .activateConversation(97)
    await flushMicrotasks()
    releaseExpanded()
    pending.resolve(settledSnapshot({ graph_revision: 2 }))
    await flushMicrotasks()
    expect(
      useWorkflowGraphStore.getState().getSnapshot(97)?.graph_revision
    ).toBe(2)
    await vi.advanceTimersByTimeAsync(10 * 60 * 1_000)
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)
    releaseOverlay()
  })

  it("refreshes every ten minutes and resets the clock after event convergence", async () => {
    getWorkflowGraphSnapshot
      .mockResolvedValueOnce(settledSnapshot({ graph_revision: 2 }))
      .mockResolvedValueOnce(settledSnapshot({ graph_revision: 4 }))
      .mockResolvedValueOnce(settledSnapshot({ graph_revision: 5 }))

    const release = useWorkflowGraphStore.getState().activateConversation(71)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)

    await vi.advanceTimersByTimeAsync(9 * 60 * 1_000)
    useWorkflowGraphStore.getState().handleGraphChanged({
      parent_conversation_id: 71,
      workflow_id: "wf-1",
      graph_revision: 3,
    })
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)

    await vi.advanceTimersByTimeAsync(9 * 60 * 1_000)
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)
    await vi.advanceTimersByTimeAsync(60 * 1_000)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(3)
    release()
  })

  it("ignores graph events for inactive detail seeds", async () => {
    useWorkflowGraphStore
      .getState()
      .applyFromDetail(72, baseSnapshot({ graph_revision: 2 }))

    useWorkflowGraphStore.getState().handleGraphChanged({
      parent_conversation_id: 72,
      workflow_id: "wf-1",
      graph_revision: 3,
    })
    useWorkflowGraphStore.getState().handleCompatibilityNudge({
      parent_conversation_id: 72,
    })
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).not.toHaveBeenCalled()

    await vi.advanceTimersByTimeAsync(10 * 60 * 1_000)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).not.toHaveBeenCalled()
  })

  it("equal and lower graph revisions neither fetch nor reset the timer", async () => {
    getWorkflowGraphSnapshot.mockResolvedValue(
      settledSnapshot({ graph_revision: 5 })
    )

    const release = useWorkflowGraphStore.getState().activateConversation(73)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)

    await vi.advanceTimersByTimeAsync(9 * 60 * 1_000)
    useWorkflowGraphStore.getState().handleGraphChanged({
      parent_conversation_id: 73,
      workflow_id: "wf-1",
      graph_revision: 5,
    })
    useWorkflowGraphStore.getState().handleGraphChanged({
      parent_conversation_id: 73,
      workflow_id: "wf-1",
      graph_revision: 4,
    })
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)

    await vi.advanceTimersByTimeAsync(60 * 1_000)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)
    release()
  })

  it("a compatibility nudge fetches only while active and resets fallback from completion", async () => {
    getWorkflowGraphSnapshot
      .mockResolvedValueOnce(settledSnapshot({ graph_revision: 1 }))
      .mockResolvedValueOnce(settledSnapshot({ graph_revision: 2 }))
      .mockResolvedValueOnce(settledSnapshot({ graph_revision: 3 }))

    const release = useWorkflowGraphStore.getState().activateConversation(74)
    await flushMicrotasks()
    await vi.advanceTimersByTimeAsync(9 * 60 * 1_000)

    useWorkflowGraphStore.getState().handleCompatibilityNudge({
      parent_conversation_id: 74,
    })
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)

    await vi.advanceTimersByTimeAsync(9 * 60 * 1_000)
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)
    await vi.advanceTimersByTimeAsync(60 * 1_000)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(3)

    release()
    useWorkflowGraphStore.getState().handleCompatibilityNudge({
      parent_conversation_id: 74,
    })
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(3)
  })

  it("one of two leases keeps event and fallback eligibility", async () => {
    getWorkflowGraphSnapshot.mockResolvedValue(settledSnapshot())

    const firstRelease = useWorkflowGraphStore
      .getState()
      .activateConversation(75)
    const finalRelease = useWorkflowGraphStore
      .getState()
      .activateConversation(75)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)

    firstRelease()
    useWorkflowGraphStore.getState().handleCompatibilityNudge({
      parent_conversation_id: 75,
    })
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)

    await vi.advanceTimersByTimeAsync(10 * 60 * 1_000)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(3)

    finalRelease()
    useWorkflowGraphStore.getState().handleCompatibilityNudge({
      parent_conversation_id: 75,
    })
    await vi.advanceTimersByTimeAsync(10 * 60 * 1_000)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(3)
  })

  it("pending-readiness duplicate leases share one fallback after the creator releases", async () => {
    const changed = deferred<() => void>()
    const nudge = deferred<() => void>()
    subscribeWorkflowGraphChanged.mockReturnValue(changed.promise)
    subscribeWorkflowCompatibilityNudge.mockReturnValue(nudge.promise)
    getWorkflowGraphSnapshot.mockResolvedValue(settledSnapshot())

    const creatorRelease = useWorkflowGraphStore
      .getState()
      .activateConversation(76)
    const remainingRelease = useWorkflowGraphStore
      .getState()
      .activateConversation(76)
    creatorRelease()

    changed.resolve(vi.fn())
    nudge.resolve(vi.fn())
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)

    await vi.advanceTimersByTimeAsync(10 * 60 * 1_000)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)

    remainingRelease()
    await vi.advanceTimersByTimeAsync(10 * 60 * 1_000)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)
  })

  it("a current response behind a newer cached revision still rearms fallback", async () => {
    const initial = deferred<WorkflowGraphSnapshot | null>()
    getWorkflowGraphSnapshot
      .mockReturnValueOnce(initial.promise)
      .mockResolvedValueOnce(settledSnapshot({ graph_revision: 10 }))

    const release = useWorkflowGraphStore.getState().activateConversation(77)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)

    useWorkflowGraphStore
      .getState()
      .applyFromDetail(77, settledSnapshot({ graph_revision: 9 }))
    initial.resolve(settledSnapshot({ graph_revision: 8 }))
    await flushMicrotasks()
    expect(
      useWorkflowGraphStore.getState().getSnapshot(77)?.graph_revision
    ).toBe(9)

    await vi.advanceTimersByTimeAsync(10 * 60 * 1_000)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)
    release()
  })

  it("stale generation completion never arms a timer", async () => {
    const stale = deferred<WorkflowGraphSnapshot | null>()
    const current = deferred<WorkflowGraphSnapshot | null>()
    getWorkflowGraphSnapshot
      .mockResolvedValueOnce(baseSnapshot({ graph_revision: 1 }))
      .mockReturnValueOnce(stale.promise)
      .mockReturnValueOnce(current.promise)

    const release = useWorkflowGraphStore.getState().activateConversation(78)
    await flushMicrotasks()
    useWorkflowGraphStore.getState().handleCompatibilityNudge({
      parent_conversation_id: 78,
    })
    useWorkflowGraphStore.getState().handleCompatibilityNudge({
      parent_conversation_id: 78,
    })
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(3)

    stale.resolve(baseSnapshot({ graph_revision: 99 }))
    await flushMicrotasks()
    await vi.advanceTimersByTimeAsync(10 * 60 * 1_000)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(3)

    current.resolve(baseSnapshot({ graph_revision: 2 }))
    await flushMicrotasks()
    release()
  })

  it("old activation epoch completion cannot arm a reactivated epoch timer", async () => {
    const oldRequest = deferred<WorkflowGraphSnapshot | null>()
    const newRequest = deferred<WorkflowGraphSnapshot | null>()
    getWorkflowGraphSnapshot
      .mockReturnValueOnce(oldRequest.promise)
      .mockReturnValueOnce(newRequest.promise)
      .mockResolvedValueOnce(settledSnapshot({ graph_revision: 3 }))

    const oldRelease = useWorkflowGraphStore.getState().activateConversation(79)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
    oldRelease()

    const changed = deferred<() => void>()
    const nudge = deferred<() => void>()
    subscribeWorkflowGraphChanged.mockReturnValue(changed.promise)
    subscribeWorkflowCompatibilityNudge.mockReturnValue(nudge.promise)
    const newRelease = useWorkflowGraphStore.getState().activateConversation(79)

    oldRequest.resolve(settledSnapshot({ graph_revision: 1 }))
    await flushMicrotasks()
    expect(
      useWorkflowGraphStore.getState().getSnapshot(79)?.graph_revision
    ).toBe(1)
    await vi.advanceTimersByTimeAsync(4_999)
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(1)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)

    changed.resolve(vi.fn())
    nudge.resolve(vi.fn())
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)

    await vi.advanceTimersByTimeAsync(9 * 60 * 1_000 + 55 * 1_000)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)
    newRequest.resolve(settledSnapshot({ graph_revision: 2 }))
    await flushMicrotasks()

    await vi.advanceTimersByTimeAsync(10 * 60 * 1_000)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(3)
    newRelease()
  })

  it("final release clears fallback and late completion cannot rearm it", async () => {
    const request = deferred<WorkflowGraphSnapshot | null>()
    getWorkflowGraphSnapshot.mockReturnValue(request.promise)

    const release = useWorkflowGraphStore.getState().activateConversation(80)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)

    release()
    request.resolve(baseSnapshot({ graph_revision: 2 }))
    await flushMicrotasks()
    expect(
      useWorkflowGraphStore.getState().getSnapshot(80)?.graph_revision
    ).toBe(2)

    await vi.advanceTimersByTimeAsync(10 * 60 * 1_000)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
  })

  it("null preserves an existing graph but remains empty without a cache", async () => {
    useWorkflowGraphStore.getState().applyFromDetail(
      81,
      settledSnapshot({
        graph_revision: null,
        compatibility: "observed_only",
        overall_state: "observed_only",
        workflow_id: null,
      })
    )
    getWorkflowGraphSnapshot.mockResolvedValue(null)

    const releaseExisting = useWorkflowGraphStore
      .getState()
      .activateConversation(81)
    const releaseEmpty = useWorkflowGraphStore
      .getState()
      .activateConversation(82)
    await flushMicrotasks()
    await flushMicrotasks()

    const existing = useWorkflowGraphStore.getState().getEntry(81)
    expect(existing?.snapshot?.compatibility).toBe("observed_only")
    expect(existing?.error).toBe("Workflow graph snapshot unavailable")
    const empty = useWorkflowGraphStore.getState().getEntry(82)
    expect(empty?.snapshot).toBeNull()
    expect(empty?.error).toBeNull()

    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)
    await vi.advanceTimersByTimeAsync(10 * 60 * 1_000)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(4)
    releaseExisting()
    releaseEmpty()
  })

  it("failed refresh retains the graph and retries only at the next interval", async () => {
    useWorkflowGraphStore
      .getState()
      .applyFromDetail(83, settledSnapshot({ graph_revision: 3 }))
    getWorkflowGraphSnapshot
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValueOnce(settledSnapshot({ graph_revision: 4 }))

    const release = useWorkflowGraphStore.getState().activateConversation(83)
    await flushMicrotasks()
    await flushMicrotasks()

    const failed = useWorkflowGraphStore.getState().getEntry(83)
    expect(failed?.snapshot?.graph_revision).toBe(3)
    expect(failed?.loading).toBe(false)
    expect(failed?.error).toBe("offline")
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)

    await vi.advanceTimersByTimeAsync(599_999)
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(1)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)
    release()
  })

  it("retains the exact cached snapshot while an accepted refresh is loading", async () => {
    const seeded = baseSnapshot({
      graph_revision: 3,
      workflow_id: "wf-seed-loading",
    })
    useWorkflowGraphStore.getState().applyFromDetail(86, seeded)
    const retainedBeforeRefresh = useWorkflowGraphStore
      .getState()
      .getSnapshot(86)

    const pending = deferred<WorkflowGraphSnapshot | null>()
    getWorkflowGraphSnapshot.mockReturnValueOnce(pending.promise)

    const release = useWorkflowGraphStore.getState().activateConversation(86)
    await flushMicrotasks()

    const loading = useWorkflowGraphStore.getState().getEntry(86)
    expect(loading?.loading).toBe(true)
    expect(loading?.snapshot).toBe(retainedBeforeRefresh)
    expect(loading?.snapshot).toBe(seeded)
    expect(loading?.snapshot?.graph_revision).toBe(3)
    expect(loading?.snapshot?.workflow_id).toBe("wf-seed-loading")
    expect(loading?.error).toBeNull()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledWith(86)

    pending.resolve(
      baseSnapshot({ graph_revision: 4, workflow_id: "wf-fresh-loading" })
    )
    await flushMicrotasks()

    const done = useWorkflowGraphStore.getState().getEntry(86)
    expect(done?.loading).toBe(false)
    expect(done?.snapshot?.graph_revision).toBe(4)
    expect(done?.snapshot?.workflow_id).toBe("wf-fresh-loading")
    expect(done?.error).toBeNull()
    release()
  })

  it("failed and stale compatibility completions follow the common scheduler", async () => {
    const stale = deferred<WorkflowGraphSnapshot | null>()
    const current = deferred<WorkflowGraphSnapshot | null>()
    const postRelease = deferred<WorkflowGraphSnapshot | null>()
    getWorkflowGraphSnapshot
      .mockResolvedValueOnce(settledSnapshot({ graph_revision: 1 }))
      .mockRejectedValueOnce(new Error("nudge offline"))
      .mockResolvedValueOnce(settledSnapshot({ graph_revision: 2 }))
      .mockReturnValueOnce(stale.promise)
      .mockReturnValueOnce(current.promise)
      .mockReturnValueOnce(postRelease.promise)

    const release = useWorkflowGraphStore.getState().activateConversation(84)
    await flushMicrotasks()
    useWorkflowGraphStore.getState().handleCompatibilityNudge({
      parent_conversation_id: 84,
    })
    await flushMicrotasks()
    expect(useWorkflowGraphStore.getState().getEntry(84)?.error).toBe(
      "nudge offline"
    )

    await vi.advanceTimersByTimeAsync(10 * 60 * 1_000)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(3)

    useWorkflowGraphStore.getState().handleCompatibilityNudge({
      parent_conversation_id: 84,
    })
    useWorkflowGraphStore.getState().handleCompatibilityNudge({
      parent_conversation_id: 84,
    })
    stale.resolve(settledSnapshot({ graph_revision: 99 }))
    await flushMicrotasks()
    await vi.advanceTimersByTimeAsync(10 * 60 * 1_000)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(5)

    current.resolve(settledSnapshot({ graph_revision: 3 }))
    await flushMicrotasks()
    useWorkflowGraphStore.getState().handleCompatibilityNudge({
      parent_conversation_id: 84,
    })
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(6)
    release()
    postRelease.resolve(settledSnapshot({ graph_revision: 4 }))
    await flushMicrotasks()
    await vi.advanceTimersByTimeAsync(10 * 60 * 1_000)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(6)
  })

  it("subscription failures still allow initial and periodic refresh", async () => {
    const pendingChanged = deferred<() => void>()
    const pendingNudge = deferred<() => void>()
    subscribeWorkflowGraphChanged
      .mockRejectedValueOnce(new Error("graph events unavailable"))
      .mockReturnValueOnce(pendingChanged.promise)
    subscribeWorkflowCompatibilityNudge
      .mockRejectedValueOnce(new Error("nudge events unavailable"))
      .mockReturnValueOnce(pendingNudge.promise)
    getWorkflowGraphSnapshot.mockResolvedValue(
      settledSnapshot({ graph_revision: 2 })
    )

    const release = useWorkflowGraphStore.getState().activateConversation(85)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)

    await vi.advanceTimersByTimeAsync(10 * 60 * 1_000)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)
    expect(subscribeWorkflowGraphChanged).toHaveBeenCalledTimes(2)
    expect(subscribeWorkflowCompatibilityNudge).toHaveBeenCalledTimes(2)
    release()
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
