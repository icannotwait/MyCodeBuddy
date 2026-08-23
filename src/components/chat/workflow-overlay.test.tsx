import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

import { SubAgentOverlay } from "./sub-agent-overlay"
import { WorkflowDagCanvas } from "./workflow-dag-canvas"
import { WorkflowGraphPanel } from "./workflow-graph-panel"
import enMessages from "@/i18n/messages/en.json"
import { getWorkflowGraphSnapshot, resolveCompletionDecision } from "@/lib/api"
import { openDelegatedChildSession } from "@/lib/open-delegated-child-session"
import type {
  CompletionProjectionV2,
  WorkflowGraphSnapshot,
  WorkflowNodeSnapshot,
} from "@/lib/types"
import {
  __resetWorkflowGraphStoreForTests,
  useWorkflowGraphStore,
} from "@/lib/workflow-graph-store"

const {
  getWorkspaceStateStore,
  openWorkflowFile,
  workspaceAcquire,
  workspaceRelease,
  workspaceSubscribeEnvelopes,
  workspaceToken,
  workspaceUnsubscribe,
} = vi.hoisted(() => {
  const unsubscribe = vi.fn()
  return {
    getWorkspaceStateStore: vi.fn(),
    openWorkflowFile: vi.fn(async () => {}),
    workspaceAcquire: vi.fn(),
    workspaceRelease: vi.fn(),
    workspaceSubscribeEnvelopes: vi.fn(() => unsubscribe),
    workspaceToken: { mode: "paths" as const },
    workspaceUnsubscribe: unsubscribe,
  }
})

vi.mock("@/components/ai-elements/link-safety", () => ({
  useOpenLinkOrFile: () => openWorkflowFile,
}))

vi.mock("@/hooks/use-workspace-state-store", () => ({
  getWorkspaceStateStore,
}))

vi.mock("@/hooks/use-delegated-sub-session", () => ({
  useDelegatedSubSession: vi.fn(() => ({
    binding: undefined,
    detail: null,
    loading: false,
    error: null,
  })),
}))

vi.mock("@/contexts/acp-connections-context", async () => {
  const actual = await vi.importActual<
    typeof import("@/contexts/acp-connections-context")
  >("@/contexts/acp-connections-context")
  return {
    ...actual,
    useConnectionStore: () => ({
      subscribeKey: () => () => {},
      getConnection: () => undefined,
      getActiveKey: () => null,
      subscribeActiveKey: () => () => {},
    }),
  }
})

vi.mock("@/lib/open-delegated-child-session", () => ({
  openDelegatedChildSession: vi.fn(async () => true),
}))

vi.mock("@/lib/api", async () => {
  const actual = await vi.importActual<typeof import("@/lib/api")>("@/lib/api")
  return {
    ...actual,
    getWorkflowGraphSnapshot: vi.fn(async () => null),
    resolveCompletionDecision: vi.fn(),
    subscribeWorkflowGraphChanged: vi.fn(async () => () => {}),
    subscribeWorkflowCompatibilityNudge: vi.fn(async () => () => {}),
  }
})

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
    is_observed: false,
    retained_observed: false,
    required: true,
    node_outcome: null,
    deps: [],
    ...overrides,
  }
}

function skeletonGraph(): WorkflowGraphSnapshot {
  return {
    schema_version: 1,
    workflow_id: "wf-test",
    workflow_kind: "brainstorm_to_delivery",
    manifest_revision: 1,
    graph_revision: 1,
    manifest_state: "estimated",
    compatibility: "manifest",
    overall_state: "estimated",
    projection_warning_codes: [],
    current_phase_id: "design",
    current_node_ids: [],
    phases: [
      { id: "design", kind: "design", title: "Design" },
      { id: "plan", kind: "plan", title: "Plan" },
      { id: "tasks", kind: "tasks", title: "Tasks" },
      { id: "final", kind: "final", title: "Final" },
    ],
    nodes: [
      node({
        node_id: "n-est",
        phase_id: "plan",
        role: "reviewer",
        status: "estimated",
        title: "Plan reviewer",
        required: true,
        is_observed: false,
      }),
      node({
        node_id: "n-req",
        phase_id: "plan",
        role: "reviewer",
        status: "running",
        title: "Required reviewer",
        required: true,
        is_observed: true,
        latest_child_conversation_id: 77,
        agent_type: "codex",
        run_count: 1,
        round_count: 0,
      }),
      node({
        node_id: "n-opt",
        phase_id: "plan",
        role: "reviewer",
        status: "completed",
        title: "Optional reviewer",
        required: false,
        is_observed: true,
        latest_child_conversation_id: 88,
      }),
    ],
    edges: [{ from: "n-est", to: "n-req" }],
    gates: [
      {
        gate_id: "g-plan",
        gate_kind: "plan",
        resolution_mode: "parent_adjudication",
        required_reviewer_node_ids: ["n-est", "n-req"],
        required_count: 2,
        returned_count: 0,
        running_count: 1,
        blocked_count: 0,
      },
    ],
  }
}

/** Single fixture covering AC3/AC5/AC6/AC7 vocabulary in one acceptance case. */
function fullVocabularyGraph(): WorkflowGraphSnapshot {
  return {
    ...skeletonGraph(),
    schema_version: 2,
    graph_revision: 50,
    current_phase_id: "tasks",
    current_node_ids: ["task-review-req"],
    nodes: [
      node({
        node_id: "task-impl",
        phase_id: "tasks",
        role: "implementer",
        agent_type: "codex",
        model: "gpt-5.2",
        effort: "high",
        task_index: 1,
        status: "completed",
        title: "Full vocab implementer",
        is_observed: true,
        latest_child_conversation_id: 201,
        run_count: 2,
        replacement_count: 1,
        round_count: 3,
        active_child_generation: 1,
        gate_cycle: 2,
        started_at: "2026-07-19T00:00:00.000Z",
        finished_at: "2026-07-19T00:05:30.000Z",
        // Two finished generations: 5m30s + 2m = 7m30s total.
        elapsed_completed_ms: 7 * 60_000 + 30_000,
        tool_call_count: 12,
        edit_tool_call_count: 3,
        touched_file_count: 4,
        touched_files_truncated: false,
        additions: 10,
        deletions: 2,
        line_counts_complete: true,
        task_risk_level: "high",
        task_risk_reason_codes: ["security_trust_boundary", "shared_interface"],
        required_reviewer_count: 1,
        returned_reviewer_count: 0,
        ...({
          task_risk_reason:
            "Touches D:/private/project/src/security-boundary.ts",
          task_risk_evidence: ["src/security-boundary.ts"],
        } as Record<string, unknown>),
      }),
      node({
        node_id: "task-review-req",
        phase_id: "tasks",
        role: "reviewer",
        agent_type: "grok",
        task_index: 1,
        status: "running",
        title: "Required task reviewer",
        required: true,
        is_observed: true,
        latest_child_conversation_id: 202,
        run_count: 1,
        replacement_count: 0,
        round_count: 1,
        task_risk_level: "high",
        task_risk_reason_codes: ["security_trust_boundary", "shared_interface"],
        required_reviewer_count: 1,
        returned_reviewer_count: 0,
      }),
      node({
        node_id: "task-review-opt",
        phase_id: "tasks",
        role: "reviewer",
        agent_type: "claude",
        task_index: 1,
        status: "estimated",
        title: "Optional task reviewer",
        required: false,
        is_observed: false,
        run_count: 0,
        replacement_count: 0,
      }),
    ],
    edges: [
      { from: "task-impl", to: "task-review-req" },
      { from: "task-impl", to: "task-review-opt" },
    ],
    gates: [],
  }
}

function adaptiveTaskGraph(
  returnedReviewerCount: number,
  riskLevel: "normal" | "high" = "high"
): WorkflowGraphSnapshot {
  const reviewerCount = riskLevel === "high" ? 2 : 1
  const reviewers = [
    node({
      node_id: "task-review-grok",
      phase_id: "tasks",
      role: "reviewer",
      task_index: 1,
      status: "completed",
      title: "Grok reviewer with a deliberately long operational title",
      agent_type: "grok",
      is_observed: true,
      latest_child_conversation_id: 92,
      task_risk_level: riskLevel,
      task_risk_reason_codes:
        riskLevel === "high"
          ? ["security_trust_boundary", "shared_interface"]
          : ["shared_interface"],
      required_reviewer_count: reviewerCount,
      returned_reviewer_count: returnedReviewerCount,
    }),
    ...(riskLevel === "high"
      ? [
          node({
            node_id: "task-review-codex",
            phase_id: "tasks",
            role: "reviewer",
            task_index: 1,
            status: returnedReviewerCount === 2 ? "completed" : "running",
            title: "Codex reviewer",
            agent_type: "codex",
            is_observed: true,
            latest_child_conversation_id: 93,
            task_risk_level: riskLevel,
            task_risk_reason_codes: [
              "security_trust_boundary",
              "shared_interface",
            ],
            required_reviewer_count: reviewerCount,
            returned_reviewer_count: returnedReviewerCount,
          }),
        ]
      : []),
  ]

  return {
    ...skeletonGraph(),
    schema_version: 2,
    graph_revision: returnedReviewerCount + 1,
    current_phase_id: "tasks",
    current_node_ids:
      returnedReviewerCount === reviewerCount ? [] : ["task-review-codex"],
    nodes: [
      node({
        node_id: "plan-review-grok",
        phase_id: "plan",
        role: "reviewer",
        title: "Plan reviewer",
      }),
      node({
        node_id: "plan-author",
        phase_id: "plan",
        role: "author",
        title: "Plan Author",
      }),
      node({
        node_id: "task-impl",
        phase_id: "tasks",
        role: "implementer",
        task_index: 1,
        status: "completed",
        title: "Adaptive routing implementation",
        is_observed: true,
        latest_child_conversation_id: 91,
        task_risk_level: riskLevel,
        task_risk_reason_codes:
          riskLevel === "high"
            ? [
                "security_trust_boundary",
                "shared_interface",
                "src/security-boundary.ts",
              ]
            : ["shared_interface"],
        required_reviewer_count: reviewerCount,
        returned_reviewer_count: returnedReviewerCount,
        ...({
          task_risk_reason:
            "Touches D:/private/project/src/security-boundary.ts",
          task_risk_evidence: ["src/security-boundary.ts"],
        } as Record<string, unknown>),
      }),
      ...reviewers,
    ],
    edges: [],
    gates: [],
  }
}

function renderWithIntl(
  ui: React.ReactElement,
  messages: typeof enMessages = enMessages
) {
  return render(
    <NextIntlClientProvider locale="en" messages={messages}>
      {ui}
    </NextIntlClientProvider>
  )
}

type ObservedDagResize = {
  callback: ResizeObserverCallback
  observer: ResizeObserver
  target: Element
}

let observedDagResizes: ObservedDagResize[] = []

class DagResizeObserver implements ResizeObserver {
  private readonly callback: ResizeObserverCallback

  constructor(callback: ResizeObserverCallback) {
    this.callback = callback
  }

  observe(target: Element): void {
    observedDagResizes.push({
      callback: this.callback,
      observer: this,
      target,
    })
  }

  unobserve(target: Element): void {
    observedDagResizes = observedDagResizes.filter(
      (entry) => entry.target !== target || entry.observer !== this
    )
  }

  disconnect(): void {
    observedDagResizes = observedDagResizes.filter(
      (entry) => entry.observer !== this
    )
  }
}

function publishDagWidth(width: number): void {
  act(() => {
    for (const entry of observedDagResizes) {
      entry.callback(
        [
          {
            target: entry.target,
            contentRect: { width } as DOMRectReadOnly,
          } as ResizeObserverEntry,
        ],
        entry.observer
      )
    }
  })
}

function workspaceStoreDouble() {
  const token = { mode: "paths" as const }
  const unsubscribe = vi.fn()
  return {
    token,
    unsubscribe,
    store: {
      acquire: vi.fn(() => token),
      release: vi.fn(),
      subscribeEnvelopes: vi.fn(() => unsubscribe),
    },
  }
}

const completionCas = {
  attention_id: "attention-task-9",
  task_id: "task-9",
  kind: "completion_decision" as const,
  captured_scope_digest: `sha256:${"a".repeat(64)}`,
  latest_run_id: "task-9",
  node_id: "task-implementer",
}

function completionProjection(
  overrides: Partial<CompletionProjectionV2> = {}
): CompletionProjectionV2 {
  return {
    protocol_version: 2,
    graph_revision: 1,
    card: {
      state: "needs_decision",
      role: "implementer",
      outcome: null,
      summary: "Choose the implementation outcome.",
      report_file: null,
      source: null,
      evidence_validated: false,
      attention: completionCas,
    },
    ...overrides,
  }
}

function archivedGraph(
  overrides: Partial<WorkflowGraphSnapshot> = {}
): WorkflowGraphSnapshot {
  const graph = skeletonGraph()
  return {
    ...graph,
    completion: completionProjection({
      card: {
        ...completionProjection().card,
        summary: "Historical implementation report",
        report_file: "reports/task-7.md",
        evidence_validated: true,
      },
    }),
    completion_protocol: {
      version: 2,
      mode: "v2_enforce",
      creation_mode: "v2_enforce",
      legacy_source: null,
      v2_successor: null,
      read_only_reason: null,
      automatic_root_wake: false,
    },
    archived: {
      source_conversation_id: 42,
      plan_rel_path: "docs/superpowers/plans/archive.md",
      successor_conversation_id: null,
      can_create_simple_successor: false,
    },
    ...overrides,
  }
}

function simpleGraph(): WorkflowGraphSnapshot {
  return {
    schema_version: 1,
    workflow_kind: "brainstorm_to_delivery",
    compatibility: "simple",
    overall_state: "blocked",
    simple: {
      plan_rel_path: "docs/superpowers/plans/simple.md",
      progress_rel_path: ".superpowers/sdd/42/progress.md",
    },
    projection_warning_codes: ["simple_progress_block_missing"],
    current_phase_id: "tasks",
    current_node_ids: ["simple-task-5"],
    phases: [{ id: "tasks", kind: "tasks", title: null }],
    nodes: [
      node({
        node_id: "simple-task-1",
        kind: "task",
        phase_id: "tasks",
        task_index: 1,
        title: "Pending task",
        status: "pending",
      }),
      node({
        node_id: "simple-task-2",
        kind: "task",
        phase_id: "tasks",
        task_index: 2,
        title: "Declared in-progress task",
        status: "in_progress",
      }),
      node({
        node_id: "simple-task-3",
        kind: "task",
        phase_id: "tasks",
        task_index: 3,
        title: "Completed task",
        status: "completed",
      }),
      node({
        node_id: "simple-task-4",
        kind: "task",
        phase_id: "tasks",
        task_index: 4,
        title: "Blocked task",
        status: "blocked",
        sync_state: "out_of_sync",
        projection_warning_codes: ["simple_completed_task_missing_commit"],
      }),
      node({
        node_id: "simple-task-5",
        kind: "task",
        phase_id: "tasks",
        task_index: 5,
        title: "Live child activity",
        status: "running",
        latest_run_status: "running",
        latest_child_conversation_id: 105,
        latest_task_id: "task-live",
        is_observed: true,
        run_count: 1,
        started_at: "2026-08-11T00:00:00.000Z",
        tool_call_count: 4,
      }),
    ],
    edges: [
      { from: "simple-task-1", to: "simple-task-2" },
      { from: "simple-task-2", to: "simple-task-3" },
    ],
    gates: [
      {
        gate_id: "must-not-render",
        gate_kind: "tasks",
        resolution_mode: "parent_adjudication",
        required_reviewer_node_ids: ["simple-task-1"],
        required_count: 1,
        returned_count: 0,
        running_count: 0,
        blocked_count: 0,
      },
    ],
    completion: completionProjection(),
  }
}

function simpleDagGraph(): WorkflowGraphSnapshot {
  return {
    schema_version: 1,
    workflow_kind: "brainstorm_to_delivery",
    compatibility: "simple",
    overall_state: "in_progress",
    simple: {
      plan_rel_path: "docs/superpowers/plans/dag.md",
      progress_rel_path: ".superpowers/sdd/88/progress.md",
    },
    projection_warning_codes: ["simple_progress_block_missing"],
    current_phase_id: "tasks",
    current_node_ids: ["t2-primary", "t2-aux"],
    phases: [{ id: "tasks", kind: "tasks", title: null }],
    nodes: [
      node({
        node_id: "t1-impl",
        kind: "task",
        phase_id: "tasks",
        role: "implementer",
        agent_type: "codex",
        task_index: 1,
        title: "Task 1 implementation",
        status: "completed",
        is_observed: true,
        latest_child_conversation_id: 101,
      }),
      node({
        node_id: "t1-primary",
        kind: "task",
        phase_id: "tasks",
        role: "reviewer",
        agent_type: "codex",
        task_index: 1,
        title: "Task 1 primary review",
        status: "completed",
        is_observed: true,
        latest_child_conversation_id: 102,
      }),
      node({
        node_id: "t2-impl",
        kind: "task",
        phase_id: "tasks",
        role: "implementer",
        agent_type: "codex",
        task_index: 2,
        title: "Task 2 implementation",
        status: "completed",
        is_observed: true,
        latest_child_conversation_id: 201,
      }),
      node({
        node_id: "t2-primary",
        kind: "task",
        phase_id: "tasks",
        role: "reviewer",
        agent_type: "codex",
        model: "gpt-5.6",
        effort: "high",
        task_index: 2,
        title: "Task 2 primary review",
        status: "running",
        latest_run_status: "running",
        is_observed: true,
        latest_child_conversation_id: 202,
        run_count: 2,
        replacement_count: 1,
        round_count: 3,
        elapsed_completed_ms: 102_000,
        tool_call_count: 6,
        touched_file_count: 3,
        additions: 74,
        deletions: 12,
        line_counts_complete: true,
      }),
      node({
        node_id: "t2-aux",
        kind: "task",
        phase_id: "tasks",
        role: "reviewer",
        agent_type: "codex",
        task_index: 2,
        title: "Task 2 auxiliary review",
        status: "running",
        latest_run_status: "running",
        is_observed: true,
        latest_child_conversation_id: 203,
        sync_state: "out_of_sync",
        projection_warning_codes: ["simple_completed_task_missing_commit"],
      }),
      node({
        node_id: "t3-impl",
        kind: "task",
        phase_id: "tasks",
        role: "implementer",
        agent_type: "codex",
        task_index: 3,
        title: "Task 3 implementation",
        status: "estimated",
      }),
      node({
        node_id: "t3-primary",
        kind: "task",
        phase_id: "tasks",
        role: "reviewer",
        agent_type: "codex",
        task_index: 3,
        title: "Task 3 primary review",
        status: "estimated",
      }),
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
    gates: [
      {
        gate_id: "simple-gate-must-not-render",
        gate_kind: "tasks",
        resolution_mode: "parent_adjudication",
        required_reviewer_node_ids: ["t2-primary"],
        required_count: 1,
        returned_count: 0,
        running_count: 1,
        blocked_count: 0,
      },
    ],
    completion: completionProjection(),
  }
}

beforeEach(() => {
  observedDagResizes = []
  vi.stubGlobal("ResizeObserver", DagResizeObserver)
  __resetWorkflowGraphStoreForTests()
  vi.mocked(openDelegatedChildSession).mockClear()
  openWorkflowFile.mockClear()
  getWorkspaceStateStore.mockReset()
  workspaceAcquire.mockReset()
  workspaceAcquire.mockReturnValue(workspaceToken)
  workspaceRelease.mockReset()
  workspaceSubscribeEnvelopes.mockClear()
  workspaceUnsubscribe.mockClear()
  getWorkspaceStateStore.mockReturnValue({
    acquire: workspaceAcquire,
    release: workspaceRelease,
    subscribeEnvelopes: workspaceSubscribeEnvelopes,
  })
  vi.mocked(getWorkflowGraphSnapshot).mockReset()
  vi.mocked(getWorkflowGraphSnapshot).mockResolvedValue(null)
  vi.mocked(resolveCompletionDecision).mockReset()
})

afterEach(() => {
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

function renderDagCanvas(
  graph: WorkflowGraphSnapshot,
  overrides: Partial<
    Pick<
      React.ComponentProps<typeof WorkflowDagCanvas>,
      "currentNodeIds" | "selectedNodeId" | "onSelect"
    >
  > = {},
  locale = "en"
) {
  const onSelect = overrides.onSelect ?? vi.fn()
  const view = render(
    <NextIntlClientProvider locale={locale} messages={enMessages}>
      <WorkflowDagCanvas
        nodes={graph.nodes}
        edges={graph.edges}
        currentNodeIds={overrides.currentNodeIds ?? graph.current_node_ids}
        selectedNodeId={
          overrides.selectedNodeId === undefined
            ? "t2-primary"
            : overrides.selectedNodeId
        }
        detailId="test-dag-detail"
        nodeDisplayTitle={(item) => item.title?.trim() || item.node_id}
        onSelect={onSelect}
      />
    </NextIntlClientProvider>
  )
  return { ...view, onSelect }
}

function simplePanelView(
  snapshot: WorkflowGraphSnapshot,
  conversationId: number | null
) {
  return (
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <WorkflowGraphPanel
        snapshot={snapshot}
        conversationId={conversationId}
        workspaceRootPath="/repo"
      />
    </NextIntlClientProvider>
  )
}

describe("WorkflowDagCanvas", () => {
  it("stays measurable while width is zero, then renders layered edges and buttons", () => {
    const graph = simpleDagGraph()
    const { onSelect } = renderDagCanvas(graph)

    const canvas = screen.getByTestId("workflow-dag-canvas")
    expect(canvas).toHaveAttribute("role", "group")
    expect(canvas).toHaveAttribute("aria-label", "Task dependency graph")
    expect(canvas).toHaveAttribute("aria-busy", "true")
    expect(screen.queryByTestId("workflow-dag-svg")).not.toBeInTheDocument()
    expect(screen.queryByTestId("workflow-dag-error")).not.toBeInTheDocument()

    publishDagWidth(288)

    expect(canvas).toHaveAttribute("aria-busy", "false")
    const svg = screen.getByTestId("workflow-dag-svg")
    expect(svg).toHaveAttribute("aria-hidden", "true")
    expect(svg).toHaveClass("pointer-events-none")
    expect(screen.getAllByTestId(/^workflow-dag-edge-/)).toHaveLength(7)
    const edge = screen.getByTestId("workflow-dag-edge-2")
    expect(edge).toHaveAttribute("data-from", "t2-impl")
    expect(edge).toHaveAttribute("data-to", "t2-primary")
    expect(edge).toHaveAttribute("data-edge-id", "e-3")
    expect(screen.getByTestId("workflow-dag-edge-6")).not.toHaveAttribute(
      "data-edge-id"
    )
    const marker = svg.querySelector("marker")
    expect(marker?.id).toMatch(/^[A-Za-z0-9_-]+$/)
    expect(marker).toHaveAttribute("aria-hidden", "true")
    expect(edge).toHaveAttribute("marker-end", `url(#${marker!.id})`)

    const selected = screen.getByTestId("workflow-dag-node-t2-primary")
    expect(selected).toHaveAttribute("aria-pressed", "true")
    expect(selected).toHaveAttribute("aria-controls", "test-dag-detail")
    expect(selected).toHaveAttribute("data-status", "running")
    expect(selected).toHaveAttribute("data-selected", "true")
    expect(selected).toHaveAccessibleName(/Current workflow node/)
    expect(selected).not.toHaveAttribute("aria-current")
    expect(within(selected).getByText("Task 2 primary review")).toHaveAttribute(
      "dir",
      "auto"
    )
    const auxiliary = screen.getByTestId("workflow-dag-node-t2-aux")
    expect(auxiliary).toHaveAttribute("data-current", "true")
    expect(auxiliary).toHaveAttribute("data-sync-state", "out_of_sync")
    expect(auxiliary).toHaveAccessibleName(/Task status is out of sync/)
    expect(screen.getByTestId("workflow-dag-node-t3-impl")).toHaveAttribute(
      "data-estimated",
      "true"
    )
    fireEvent.click(auxiliary)
    expect(onSelect).toHaveBeenCalledWith("t2-aux")

    publishDagWidth(0)
    expect(screen.getByTestId("workflow-dag-canvas")).toHaveAttribute(
      "aria-busy",
      "true"
    )
    expect(screen.queryByTestId("workflow-dag-svg")).not.toBeInTheDocument()
  })

  it("derives screen-reader relationships from edges, not node.deps", () => {
    const graph = simpleDagGraph()
    graph.nodes = graph.nodes.map((item) => ({
      ...item,
      deps: ["incorrect-secondary-authority"],
    }))
    renderDagCanvas(graph)
    publishDagWidth(288)

    const button = screen.getByTestId("workflow-dag-node-t3-impl")
    const descriptionId = button.getAttribute("aria-describedby")
    expect(descriptionId).toBeTruthy()
    expect(descriptionId).not.toContain("t3-impl")
    const description = document.getElementById(descriptionId!)
    expect(description).toHaveClass("sr-only")
    expect(description).toHaveTextContent("Depends on Task 2 primary review")
    expect(description).toHaveTextContent("Task 2 auxiliary review")
    expect(description).toHaveTextContent("Required by Task 3 primary review")
    expect(description).not.toHaveTextContent("incorrect-secondary-authority")
  })

  it("mirrors node coordinates from the Arabic locale", () => {
    renderDagCanvas(simpleDagGraph(), {}, "ar")
    publishDagWidth(288)

    expect(screen.getByTestId("workflow-dag-canvas")).toHaveAttribute(
      "dir",
      "rtl"
    )
    expect(screen.getByTestId("workflow-dag-node-t2-primary")).toHaveStyle({
      left: "150px",
    })
    expect(screen.getByTestId("workflow-dag-node-t2-aux")).toHaveStyle({
      left: "8px",
    })
  })

  it("uses synchronous and window-resize measurement without ResizeObserver", () => {
    let width = 224
    vi.stubGlobal("ResizeObserver", undefined)
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(
      () => ({ width }) as DOMRect
    )
    renderDagCanvas(simpleDagGraph())

    expect(screen.getByTestId("workflow-dag-node-t2-primary")).toHaveStyle({
      width: "98px",
    })
    width = 448
    fireEvent(window, new Event("resize"))
    expect(screen.getByTestId("workflow-dag-node-t2-primary")).toHaveStyle({
      width: "148px",
    })
  })

  it("renders a bounded fallback without partial SVG for invalid topology", () => {
    const graph = simpleDagGraph()
    graph.edges = [...graph.edges, { from: "t3-primary", to: "t1-impl" }]
    const { onSelect } = renderDagCanvas(graph)
    publishDagWidth(288)

    const error = screen.getByTestId("workflow-dag-error")
    expect(error).toHaveAttribute("data-layout-error", "cycle")
    expect(error).toHaveAttribute("role", "status")
    expect(error).toHaveTextContent("Task dependencies could not be displayed")
    const fallback = screen.getByTestId("workflow-dag-fallback")
    expect(fallback).toHaveAttribute(
      "aria-label",
      "Tasks without dependency layout"
    )
    const fallbackButtons = within(fallback).getAllByRole("button")
    expect(
      fallbackButtons.map((button) => button.getAttribute("title"))
    ).toEqual(graph.nodes.map((node) => node.title))
    for (const button of fallbackButtons) {
      expect(button).not.toHaveAttribute("aria-describedby")
    }
    fireEvent.click(screen.getByTestId("workflow-dag-node-t2-aux"))
    expect(onSelect).toHaveBeenCalledWith("t2-aux")
    expect(screen.queryByTestId("workflow-dag-svg")).not.toBeInTheDocument()
  })

  it("keeps duplicate IDs non-interactive", () => {
    const graph = simpleDagGraph()
    graph.nodes = [graph.nodes[0], { ...graph.nodes[0] }]
    graph.edges = []
    renderDagCanvas(graph, {
      currentNodeIds: ["t1-impl"],
      selectedNodeId: "t1-impl",
    })
    publishDagWidth(288)

    expect(screen.getByTestId("workflow-dag-error")).toHaveAttribute(
      "data-layout-error",
      "duplicate_node"
    )
    expect(
      within(screen.getByTestId("workflow-dag-fallback")).queryAllByRole(
        "button"
      )
    ).toHaveLength(0)
    const summaries = within(
      screen.getByTestId("workflow-dag-fallback")
    ).getAllByTitle("Task 1 implementation")
    expect(summaries).toHaveLength(2)
    for (const summary of summaries) {
      expect(summary).toHaveAttribute("data-current", "false")
      expect(summary).toHaveAttribute("data-selected", "false")
      expect(summary).not.toHaveClass("border-s-2")
      expect(summary).not.toHaveClass("ring-2")
    }
  })
})

describe("Simple workflow DAG integration", () => {
  it("renders only the Tasks DAG and one complete selected-node detail", async () => {
    renderWithIntl(
      <div className="w-72">
        <SubAgentOverlay
          delegations={[]}
          conversationId={88}
          workflowGraph={simpleDagGraph()}
          workspaceRootPath="/repo"
        />
      </div>
    )
    publishDagWidth(288)

    expect(screen.getByTestId("workflow-graph-panel")).toHaveAttribute(
      "data-compatibility",
      "simple"
    )
    const tasks = screen.getByTestId("workflow-graph-lane-tasks")
    expect(within(tasks).getByText("Tasks")).toBeVisible()
    expect(within(tasks).getByText("Current")).toBeVisible()
    expect(within(tasks).getByText("Task 2 / 3")).toBeVisible()
    expect(screen.getAllByTestId(/^workflow-dag-node-/)).toHaveLength(7)
    const edgePaths = screen.getAllByTestId(/^workflow-dag-edge-/)
    expect(edgePaths).toHaveLength(7)
    expect(new Set(edgePaths.map((path) => path.getAttribute("d"))).size).toBe(
      7
    )
    expect(screen.queryByTestId(/^simple-task-row-/)).not.toBeInTheDocument()
    expect(
      screen.queryByTestId("workflow-graph-lane-design")
    ).not.toBeInTheDocument()
    expect(
      screen.queryByTestId("workflow-graph-lane-plan")
    ).not.toBeInTheDocument()
    expect(
      screen.queryByTestId("workflow-graph-lane-final")
    ).not.toBeInTheDocument()
    expect(screen.queryByTestId("workflow-graph-edges")).not.toBeInTheDocument()
    expect(
      screen.queryByTestId("workflow-dependencies-toggle")
    ).not.toBeInTheDocument()
    expect(
      screen.queryByTestId("workflow-expand-toggle")
    ).not.toBeInTheDocument()
    expect(
      screen.queryByTestId("workflow-lane-toggle-tasks")
    ).not.toBeInTheDocument()

    const currentButtons = [
      screen.getByTestId("workflow-dag-node-t2-primary"),
      screen.getByTestId("workflow-dag-node-t2-aux"),
    ]
    for (const button of currentButtons) {
      expect(button).toHaveAttribute("data-current", "true")
    }
    expect(
      screen
        .getAllByTestId(/^workflow-dag-node-/)
        .filter((button) => button.getAttribute("aria-pressed") === "true")
    ).toHaveLength(1)

    expect(screen.getAllByTestId("workflow-dag-detail")).toHaveLength(1)
    let detail = screen.getByTestId("workflow-dag-detail")
    expect(detail.id).not.toBe("")
    expect(detail).toHaveAttribute("role", "region")
    expect(detail).toHaveAttribute("aria-label", "Selected workflow node")
    expect(detail).not.toHaveAttribute("aria-live")
    expect(
      within(detail).getByRole("heading", { name: "Task 2 primary review" })
    ).toHaveAttribute("dir", "auto")
    expect(screen.getByTestId("workflow-dag-node-t2-primary")).toHaveAttribute(
      "aria-controls",
      detail.id
    )
    expect(detail).toHaveTextContent("Task 2 primary review")
    expect(detail).toHaveTextContent("Task 2")
    expect(detail).toHaveTextContent("Role: reviewer")
    expect(detail).toHaveTextContent("Agent: Codex")
    expect(detail).toHaveTextContent("Model: gpt-5.6")
    expect(detail).toHaveTextContent("Effort: high")
    expect(detail).toHaveTextContent("Running")
    expect(detail).toHaveTextContent("Live run: Running")
    expect(detail).toHaveTextContent("1m 42s")
    expect(detail).toHaveTextContent("6 tool uses")
    expect(detail).toHaveTextContent("3 files")
    expect(detail).toHaveTextContent("+74 -12")
    expect(detail).toHaveTextContent("Runs 2")
    expect(detail).toHaveTextContent("Replacements 1")
    expect(detail).toHaveTextContent("Round 3")
    expect(
      within(detail).queryByText(/Depends on|Required by|依赖|解锁/)
    ).not.toBeInTheDocument()

    await userEvent.click(screen.getByTestId("workflow-dag-node-t2-aux"))
    expect(openDelegatedChildSession).not.toHaveBeenCalled()
    detail = screen.getByTestId("workflow-dag-detail")
    expect(detail).toHaveTextContent("Task 2 auxiliary review")
    expect(detail).toHaveTextContent("Task status is out of sync")

    await userEvent.click(screen.getByTestId("workflow-dag-node-t3-impl"))
    detail = screen.getByTestId("workflow-dag-detail")
    expect(detail).toHaveTextContent("Estimated — no session yet")
    expect(
      within(detail).queryByRole("button", { name: "Open conversation" })
    ).not.toBeInTheDocument()

    await userEvent.click(screen.getByTestId("workflow-dag-node-t2-primary"))
    await userEvent.click(screen.getByTestId("simple-task-open-t2-primary"))
    expect(openDelegatedChildSession).toHaveBeenCalledWith({
      childConversationId: 202,
      agentType: "codex",
      title: "Task 2 primary review",
    })

    expect(screen.getByText("Partial Simple projection")).toBeVisible()
    expect(screen.queryByText("Reviewer cohort")).not.toBeInTheDocument()
    expect(screen.queryByText("Decision required")).not.toBeInTheDocument()
    expect(screen.queryByText("Evidence validated")).not.toBeInTheDocument()
    expect(screen.queryByText("0 / 1")).not.toBeInTheDocument()
  })

  it("tracks current selection until a user choice, repairs removal, and resets by conversation", () => {
    const graph = {
      ...simpleDagGraph(),
      current_node_ids: ["stale-node", "t2-primary", "t2-aux"],
    }
    const view = render(simplePanelView(graph, 88))
    publishDagWidth(288)
    expect(screen.getByTestId("workflow-dag-node-t2-primary")).toHaveAttribute(
      "aria-pressed",
      "true"
    )

    const automatic = { ...graph, current_node_ids: ["t2-aux"] }
    view.rerender(simplePanelView(automatic, 88))
    expect(screen.getByTestId("workflow-dag-node-t2-aux")).toHaveAttribute(
      "aria-pressed",
      "true"
    )

    fireEvent.click(screen.getByTestId("workflow-dag-node-t1-primary"))
    const refreshed = { ...automatic, current_node_ids: ["t2-primary"] }
    view.rerender(simplePanelView(refreshed, 88))
    expect(screen.getByTestId("workflow-dag-node-t1-primary")).toHaveAttribute(
      "aria-pressed",
      "true"
    )

    const removed = {
      ...refreshed,
      nodes: refreshed.nodes.filter((item) => item.node_id !== "t1-primary"),
      edges: refreshed.edges.filter(
        (edge) => edge.from !== "t1-primary" && edge.to !== "t1-primary"
      ),
    }
    view.rerender(simplePanelView(removed, 88))
    expect(screen.getByTestId("workflow-dag-node-t2-primary")).toHaveAttribute(
      "aria-pressed",
      "true"
    )

    fireEvent.click(screen.getByTestId("workflow-dag-node-t1-impl"))
    const nextConversation = {
      ...graph,
      current_node_ids: ["t2-aux"],
    }
    view.rerender(simplePanelView(nextConversation, 99))
    publishDagWidth(288)
    expect(screen.getByTestId("workflow-dag-node-t2-aux")).toHaveAttribute(
      "aria-pressed",
      "true"
    )
    expect(screen.getByTestId("workflow-dag-detail")).toHaveTextContent(
      "Task 2 auxiliary review"
    )
  })

  it.each([
    ["no", []],
    ["only stale", ["stale-node"]],
  ] as const)(
    "shows no detail for %s current IDs until the user selects",
    (_name, currentNodeIds) => {
      const graph = {
        ...simpleDagGraph(),
        current_node_ids: [...currentNodeIds],
      }
      render(simplePanelView(graph, 88))
      publishDagWidth(288)

      expect(
        screen
          .getAllByTestId(/^workflow-dag-node-/)
          .filter((button) => button.getAttribute("aria-pressed") === "true")
      ).toHaveLength(0)
      expect(
        screen.queryByTestId("workflow-dag-detail")
      ).not.toBeInTheDocument()

      fireEvent.click(screen.getByTestId("workflow-dag-node-t1-impl"))
      expect(screen.getByTestId("workflow-dag-node-t1-impl")).toHaveAttribute(
        "aria-pressed",
        "true"
      )
      expect(screen.getByTestId("workflow-dag-detail")).toHaveTextContent(
        "Task 1 implementation"
      )
    }
  )

  it("uses the locator pair as the selection scope when no conversation ID exists", () => {
    const graph = simpleDagGraph()
    const view = render(simplePanelView(graph, null))
    publishDagWidth(288)
    fireEvent.click(screen.getByTestId("workflow-dag-node-t1-impl"))

    const nextLocator = {
      ...graph,
      simple: {
        plan_rel_path: "docs/superpowers/plans/another.md",
        progress_rel_path: ".superpowers/sdd/89/progress.md",
      },
      current_node_ids: ["t2-aux"],
    }
    view.rerender(simplePanelView(nextLocator, null))
    publishDagWidth(288)

    expect(screen.getByTestId("workflow-dag-node-t2-aux")).toHaveAttribute(
      "aria-pressed",
      "true"
    )
  })

  it("keeps a missing-locator projection warning separate from graph validation", () => {
    const graph = simpleDagGraph()
    graph.simple = null
    graph.projection_warning_codes = []
    graph.nodes = graph.nodes.map((item) => ({
      ...item,
      projection_warning_codes: [],
    }))
    render(simplePanelView(graph, 88))
    publishDagWidth(288)

    expect(screen.getByTestId("simple-projection-warning")).toBeInTheDocument()
    expect(screen.getByText("Partial Simple projection")).toBeVisible()
    expect(screen.queryByTestId("workflow-dag-error")).not.toBeInTheDocument()
    expect(screen.getByTestId("workflow-dag-svg")).toBeInTheDocument()
  })

  it("does not label an observed node without a child session as estimated", () => {
    const graph = simpleDagGraph()
    graph.nodes = graph.nodes.map((item) =>
      item.node_id === "t3-primary"
        ? { ...item, status: "pending", is_observed: true }
        : item
    )
    graph.current_node_ids = ["t3-primary"]
    render(simplePanelView(graph, 88))
    publishDagWidth(288)

    const detail = screen.getByTestId("workflow-dag-detail")
    expect(detail).toHaveTextContent("No sub-agent sessions yet")
    expect(detail).not.toHaveTextContent("Estimated — no session yet")
  })

  it.each([
    {
      name: "cycle",
      error: "cycle",
      mutate(graph: WorkflowGraphSnapshot) {
        graph.edges = [...graph.edges, { from: "t3-primary", to: "t1-impl" }]
      },
    },
    {
      name: "dangling endpoint",
      error: "dangling_edge",
      mutate(graph: WorkflowGraphSnapshot) {
        graph.edges = [
          { from: "t1-impl", to: "missing-node" },
          ...graph.edges.slice(1),
        ]
      },
    },
    {
      name: "long edge",
      error: "unsupported_edge_span",
      mutate(graph: WorkflowGraphSnapshot) {
        graph.edges = [...graph.edges, { from: "t1-impl", to: "t2-impl" }]
      },
    },
  ])("shows a measured fallback for $name", ({ error, mutate }) => {
    const graph = simpleDagGraph()
    mutate(graph)
    render(simplePanelView(graph, 88))
    publishDagWidth(288)

    expect(screen.getByTestId("workflow-dag-error")).toHaveAttribute(
      "data-layout-error",
      error
    )
    expect(screen.getByTestId("workflow-dag-fallback")).toBeInTheDocument()
    expect(screen.queryByTestId("workflow-dag-svg")).not.toBeInTheDocument()
  })

  it.each(["duplicate", "blank"] as const)(
    "does not expose ambiguous selection for a %s node ID",
    (kind) => {
      const graph = simpleDagGraph()
      graph.nodes =
        kind === "duplicate"
          ? [graph.nodes[0], { ...graph.nodes[0] }]
          : [{ ...graph.nodes[0], node_id: "   " }]
      graph.edges = []
      graph.current_node_ids = []
      render(simplePanelView(graph, 88))
      publishDagWidth(288)

      expect(
        within(screen.getByTestId("workflow-dag-fallback")).queryAllByRole(
          "button"
        )
      ).toHaveLength(0)
      expect(
        screen.queryByTestId("workflow-dag-detail")
      ).not.toBeInTheDocument()
      expect(screen.queryByTestId("workflow-dag-svg")).not.toBeInTheDocument()
    }
  )

  it("uses the existing empty state without mounting a canvas", () => {
    const graph = simpleDagGraph()
    graph.nodes = []
    graph.edges = []
    graph.current_node_ids = []
    render(simplePanelView(graph, 88))

    expect(screen.getByText("No Plan tasks found")).toBeVisible()
    expect(screen.queryByTestId("workflow-dag-canvas")).not.toBeInTheDocument()
    expect(screen.queryByTestId("workflow-dag-detail")).not.toBeInTheDocument()
  })

  it("accepts a non-empty edgeless graph as one rank", () => {
    const graph = simpleDagGraph()
    graph.edges = []
    render(simplePanelView(graph, 88))
    publishDagWidth(224)

    expect(screen.getByTestId("workflow-dag-svg")).toBeInTheDocument()
    expect(screen.queryAllByTestId(/^workflow-dag-edge-/)).toHaveLength(0)
    expect(screen.getAllByTestId(/^workflow-dag-node-/)).toHaveLength(7)
    expect(screen.queryByTestId("workflow-dag-error")).not.toBeInTheDocument()
  })
})

describe("Task 7 archived and Simple workflow rendering", () => {
  it("keeps archived history navigable while removing every mutation affordance", async () => {
    const onOpenRootConversation = vi.fn()
    const graph = archivedGraph({
      archived: {
        source_conversation_id: 42,
        plan_rel_path: "docs/superpowers/plans/archive.md",
        successor_conversation_id: 84,
        can_create_simple_successor: false,
      },
    })

    renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        conversationId={42}
        workflowGraph={graph}
        workspaceRootPath={"D:\\Repo"}
        onOpenRootConversation={onOpenRootConversation}
      />
    )

    expect(screen.getByTestId("workflow-archived-banner")).toHaveTextContent(
      "Archived workflow"
    )
    await userEvent.click(screen.getByRole("button", { name: "Open Plan" }))
    expect(openWorkflowFile).toHaveBeenCalledWith(
      "D:/Repo/docs/superpowers/plans/archive.md"
    )

    expect(
      screen.queryByRole("button", { name: "Continue in Simple" })
    ).toBeNull()
    expect(
      screen.queryByRole("button", { name: "Open Simple successor" })
    ).toBeNull()
    expect(onOpenRootConversation).not.toHaveBeenCalled()

    await userEvent.click(
      screen.getByRole("button", { name: "Expand workflow graph" })
    )
    expect(screen.getByText("Historical implementation report")).toBeVisible()
    expect(screen.getByText("Report: reports/task-7.md")).toBeVisible()
    expect(
      within(screen.getByTestId("workflow-graph-lane-plan")).getByText(
        "0 / 2 · 1 running"
      )
    ).toBeVisible()
    await userEvent.click(screen.getByTestId("workflow-graph-node-open-n-req"))
    expect(openDelegatedChildSession).toHaveBeenCalledWith(
      expect.objectContaining({ childConversationId: 77 })
    )

    expect(screen.queryAllByTestId("completion-decision-card")).toHaveLength(0)
    expect(screen.queryByRole("button", { name: "Done" })).toBeNull()
    expect(screen.queryByRole("button", { name: "Retry artifact" })).toBeNull()
  })

  it.each([
    {
      successor_conversation_id: 84,
      can_create_simple_successor: false,
    },
    {
      successor_conversation_id: null,
      can_create_simple_successor: true,
    },
  ])(
    "keeps archived history visible without a successor action",
    async (compatibilityValues) => {
      const snapshot = archivedGraph()
      snapshot.archived = {
        ...snapshot.archived!,
        ...compatibilityValues,
      }
      const onOpenRootConversation = vi.fn()

      renderWithIntl(
        <SubAgentOverlay
          delegations={[]}
          workflowGraph={snapshot}
          workspaceRootPath="D:\\Repo"
          onOpenRootConversation={onOpenRootConversation}
        />
      )

      expect(screen.getByTestId("workflow-archived-banner")).toBeVisible()
      expect(screen.getByRole("button", { name: "Open Plan" })).toBeVisible()
      expect(
        screen.queryByRole("button", { name: "Continue in Simple" })
      ).toBeNull()
      expect(
        screen.queryByRole("button", { name: "Open Simple successor" })
      ).toBeNull()
      expect(onOpenRootConversation).not.toHaveBeenCalled()
    }
  )

  it("renders Simple task state, live activity, bounded warnings, and exact file links", async () => {
    const messages = structuredClone(enMessages)
    messages.Folder.chat.workflowGraph.simpleOpenProgress =
      "Open the deliberately long translated progress document label"

    renderWithIntl(
      <div className="w-72">
        <SubAgentOverlay
          delegations={[]}
          conversationId={42}
          workflowGraph={simpleGraph()}
          workspaceRootPath="/repo"
        />
      </div>,
      messages
    )
    publishDagWidth(288)

    expect(
      within(screen.getByTestId("workflow-dag-node-simple-task-1")).getByText(
        "Pending"
      )
    ).toBeVisible()
    expect(
      within(screen.getByTestId("workflow-dag-node-simple-task-2")).getByText(
        "In progress"
      )
    ).toBeVisible()
    expect(
      within(screen.getByTestId("workflow-dag-node-simple-task-3")).getByText(
        "Completed"
      )
    ).toBeVisible()
    expect(
      within(screen.getByTestId("workflow-dag-node-simple-task-4")).getByText(
        "Blocked"
      )
    ).toBeVisible()
    expect(
      within(screen.getByTestId("workflow-graph-lane-tasks")).getByText(
        "5 tasks"
      )
    ).toBeVisible()
    expect(screen.getByText("Partial Simple projection")).toBeVisible()

    await userEvent.click(screen.getByTestId("workflow-dag-node-simple-task-4"))
    expect(screen.getByTestId("workflow-dag-detail")).toHaveTextContent(
      "Task status is out of sync"
    )

    await userEvent.click(screen.getByTestId("workflow-dag-node-simple-task-5"))
    expect(screen.getByTestId("workflow-dag-detail")).toHaveTextContent(
      "Live run: Running"
    )

    await userEvent.click(screen.getByTestId("simple-task-open-simple-task-5"))
    expect(openDelegatedChildSession).toHaveBeenCalledWith(
      expect.objectContaining({ childConversationId: 105 })
    )

    await userEvent.click(screen.getByRole("button", { name: "Open Plan" }))
    await userEvent.click(screen.getByTestId("simple-progress-link"))
    expect(openWorkflowFile).toHaveBeenNthCalledWith(
      1,
      "/repo/docs/superpowers/plans/simple.md"
    )
    expect(openWorkflowFile).toHaveBeenNthCalledWith(
      2,
      "/repo/.superpowers/sdd/42/progress.md"
    )
    expect(screen.getByTestId("simple-progress-link")).toHaveClass("min-w-0")
    expect(
      screen.queryByTestId("simple-task-row-simple-task-1")
    ).not.toBeInTheDocument()

    expect(screen.queryByText("Reviewer cohort")).toBeNull()
    expect(screen.queryByText("Decision required")).toBeNull()
    expect(screen.queryByText("Evidence validated")).toBeNull()
    expect(screen.queryByText("0 / 1")).toBeNull()
  })

  it("marks a Simple projection partial when warnings exist only on tasks", () => {
    const graph = simpleGraph()
    graph.projection_warning_codes = []

    renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        conversationId={42}
        workflowGraph={graph}
        workspaceRootPath="/repo"
      />
    )

    expect(screen.getByText("Partial Simple projection")).toBeVisible()
  })

  it("releases and reacquires the Simple file watch when the conversation folder changes", async () => {
    const first = workspaceStoreDouble()
    const second = workspaceStoreDouble()
    getWorkspaceStateStore.mockImplementation((rootPath: string) => {
      if (rootPath === "C:\\First") return first.store
      if (rootPath === "D:\\Second") return second.store
      throw new Error(`Unexpected workspace root: ${rootPath}`)
    })
    const graph = simpleGraph()
    const view = (workspaceRootPath: string) => (
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <SubAgentOverlay
          delegations={[]}
          conversationId={4242}
          workflowGraph={graph}
          workspaceRootPath={workspaceRootPath}
        />
      </NextIntlClientProvider>
    )
    const { rerender, unmount } = render(view("C:\\First"))

    await waitFor(() =>
      expect(first.store.acquire).toHaveBeenCalledWith("paths")
    )
    rerender(view("D:\\Second"))
    await waitFor(() =>
      expect(second.store.acquire).toHaveBeenCalledWith("paths")
    )

    expect(first.unsubscribe).toHaveBeenCalledTimes(1)
    expect(first.store.release).toHaveBeenCalledWith(first.token)
    unmount()
    expect(second.unsubscribe).toHaveBeenCalledTimes(1)
    expect(second.store.release).toHaveBeenCalledWith(second.token)
  })
})

describe("SubAgentOverlay A13 workflow mount", () => {
  it("renders legacy history as strictly read-only", async () => {
    const onOpenRootConversation = vi.fn()
    const onResumeRoot = vi.fn()
    const graph = skeletonGraph()
    const recovery = completionProjection({
      card: {
        ...completionProjection().card,
        state: "blocked",
        attention: {
          ...completionCas,
          kind: "completion_artifact_recovery",
        },
      },
    })
    const legacy = {
      ...graph,
      completion: completionProjection(),
      nodes: graph.nodes.map((item, index) =>
        index === 0 ? { ...item, completion: recovery } : item
      ),
      completion_protocol: {
        version: 1,
        mode: "v1" as const,
        creation_mode: "v1" as const,
        legacy_source: {
          workflow_id: "wf-source",
          conversation_id: 41,
        },
        v2_successor: {
          workflow_id: "wf-successor",
          conversation_id: 99,
        },
        read_only_reason: "legacy_completion_protocol_read_only",
        automatic_root_wake: false,
      },
    } satisfies WorkflowGraphSnapshot

    renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={42}
        workflowGraph={legacy}
        defaultExpanded
        onOpenRootConversation={onOpenRootConversation}
        onResumeRoot={onResumeRoot}
      />
    )
    await userEvent.click(
      screen.getByRole("button", { name: "Expand workflow graph" })
    )

    expect(screen.getByText("Legacy workflow is read-only")).toBeInTheDocument()
    await userEvent.click(
      screen.getByRole("button", { name: "Legacy source workflow #41" })
    )
    await userEvent.click(
      screen.getByRole("button", { name: "Successor workflow #99" })
    )
    expect(onOpenRootConversation).toHaveBeenNthCalledWith(1, 41)
    expect(onOpenRootConversation).toHaveBeenNthCalledWith(2, 99)
    expect(openDelegatedChildSession).not.toHaveBeenCalled()
    expect(
      screen.queryByText("Restart with completion v2")
    ).not.toBeInTheDocument()
    expect(
      screen.queryByText("Resume root orchestration")
    ).not.toBeInTheDocument()
    expect(
      screen.queryByRole("button", { name: "Done" })
    ).not.toBeInTheDocument()
    expect(
      screen.queryByRole("button", { name: "Retry artifact" })
    ).not.toBeInTheDocument()
    expect(screen.queryAllByTestId("completion-decision-card")).toHaveLength(0)
    expect(onResumeRoot).not.toHaveBeenCalled()
    expect(resolveCompletionDecision).not.toHaveBeenCalled()
  })

  it("keeps valid v2 completion decisions and automatic wake status", async () => {
    const completion = completionProjection()
    const snapshot = {
      ...skeletonGraph(),
      completion,
      completion_protocol: {
        version: 2,
        mode: "v2_enforce" as const,
        creation_mode: "v2_enforce" as const,
        legacy_source: null,
        v2_successor: null,
        read_only_reason: null,
        automatic_root_wake: true,
      },
    } satisfies WorkflowGraphSnapshot
    vi.mocked(resolveCompletionDecision).mockResolvedValue({
      workflow_id: "wf-test",
      task_id: completionCas.task_id,
      node_id: completionCas.node_id,
      kind: completionCas.kind,
      outcome: "done",
      evidence_scope_digest: `sha256:${"b".repeat(64)}`,
      graph_revision: 2,
      idempotent_replay: false,
      completion: completionProjection({
        graph_revision: 2,
        card: {
          ...completion.card,
          state: "resolved",
          outcome: "done",
          evidence_validated: true,
          attention: null,
        },
      }),
    })

    renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={42}
        workflowGraph={snapshot}
        defaultExpanded
      />
    )
    await userEvent.click(
      screen.getByRole("button", { name: "Expand workflow graph" })
    )
    expect(
      screen.getByText("Root orchestration resumed automatically")
    ).toBeInTheDocument()
    await userEvent.click(screen.getByRole("button", { name: "Done" }))

    expect(resolveCompletionDecision).toHaveBeenCalledWith({
      cas: completionCas,
      outcome: "done",
    })
    expect(await screen.findByText("Resolved")).toBeInTheDocument()
    expect(
      screen.queryByText("Resume root orchestration")
    ).not.toBeInTheDocument()
  })

  it("first-seeds a graphless open overlay without expanding the full graph", async () => {
    vi.mocked(getWorkflowGraphSnapshot).mockResolvedValue(skeletonGraph())
    renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={94}
        workflowGraph={null}
        defaultExpanded
      />
    )

    await waitFor(() =>
      expect(screen.getByTestId("sub-agent-overlay")).toHaveAttribute(
        "data-has-workflow",
        "true"
      )
    )
    expect(screen.getByTestId("workflow-segment-workflow")).toHaveAttribute(
      "aria-selected",
      "true"
    )
    expect(screen.getByTestId("workflow-phase-rail")).toBeInTheDocument()
    expect(
      screen.getByRole("button", { name: "Expand workflow graph" })
    ).toBeInTheDocument()
  })

  it("force-refreshes once when sessions appear while still graphless", async () => {
    // Simulate the empty-mount path: interest spent a null discovery fetch
    // before any sub-agent rows exist. When sessions later open the panel,
    // force one more snapshot pull so a published workflow is not stuck behind
    // the 10-minute overlay-only fallback.
    vi.mocked(getWorkflowGraphSnapshot).mockResolvedValue(null)
    const { rerender } = renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={95}
        workflowGraph={null}
        defaultExpanded
      />
    )

    await waitFor(() => expect(getWorkflowGraphSnapshot).toHaveBeenCalled())
    const callsAfterEmptyMount = vi.mocked(getWorkflowGraphSnapshot).mock.calls
      .length
    expect(screen.queryByTestId("sub-agent-overlay")).not.toBeInTheDocument()

    vi.mocked(getWorkflowGraphSnapshot).mockResolvedValue(skeletonGraph())
    rerender(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <SubAgentOverlay
          delegations={[]}
          activities={[
            {
              origin: "native",
              authoritative: false,
              platform: "codex",
              operation: "spawn",
              observed_status: "running",
              task_id: "agent-native-1",
              started_at: "2026-08-04T00:00:00.000Z",
              updated_at: "2026-08-04T00:00:00.000Z",
            },
          ]}
          conversationId={95}
          workflowGraph={null}
          defaultExpanded
        />
      </NextIntlClientProvider>
    )

    await waitFor(() =>
      expect(screen.getByTestId("sub-agent-overlay")).toHaveAttribute(
        "data-has-workflow",
        "true"
      )
    )
    expect(
      vi.mocked(getWorkflowGraphSnapshot).mock.calls.length
    ).toBeGreaterThan(callsAfterEmptyMount)
    expect(screen.getByTestId("workflow-phase-rail")).toBeInTheDocument()
  })

  it("mounts with graph even when session/activity count is zero", () => {
    renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={42}
        workflowGraph={skeletonGraph()}
        defaultExpanded
      />
    )
    expect(screen.getByTestId("sub-agent-overlay")).toBeInTheDocument()
    expect(screen.getByTestId("sub-agent-overlay")).toHaveAttribute(
      "data-has-workflow",
      "true"
    )
    expect(screen.getByTestId("workflow-phase-rail")).toBeInTheDocument()
    expect(screen.getByTestId("workflow-sessions-segment")).toBeInTheDocument()
    expect(screen.getByTestId("workflow-segment-workflow")).toHaveAttribute(
      "aria-selected",
      "true"
    )
  })

  it("returns null with zero sessions and no graph", () => {
    const { container } = renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={1}
        workflowGraph={null}
        defaultExpanded
      />
    )
    expect(container).toBeEmptyDOMElement()
  })

  it("switches to Sessions segment and shows empty state", () => {
    renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={42}
        workflowGraph={skeletonGraph()}
        defaultExpanded
      />
    )
    fireEvent.click(screen.getByTestId("workflow-segment-sessions"))
    expect(screen.getByTestId("sub-agent-overlay")).toHaveAttribute(
      "data-segment",
      "sessions"
    )
    expect(screen.getByTestId("workflow-sessions-empty")).toBeInTheDocument()
  })

  it("keeps all four phase labels and status icons without one-line truncation classes", () => {
    renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={42}
        workflowGraph={skeletonGraph()}
        defaultExpanded
      />
    )

    const rail = screen.getByTestId("workflow-phase-rail")
    expect(within(rail).getAllByTestId("workflow-status-icon")).toHaveLength(4)
    const phaseLabels: Record<string, string> = {
      design: "Design",
      plan: "Plan",
      tasks: "Tasks",
      final: "Final",
    }
    for (const kind of ["design", "plan", "tasks", "final"] as const) {
      const label = screen
        .getByTestId(`workflow-phase-${kind}`)
        .querySelector("[data-phase-label]")
      expect(label).not.toBeNull()
      expect(label!).toHaveTextContent(phaseLabels[kind])
      expect(label!).not.toHaveClass("truncate")
      expect(label!).not.toHaveClass("line-clamp-1")
    }
  })

  it("defaults to the current phase and moves the summary when selected", () => {
    const planCurrent = { ...skeletonGraph(), current_phase_id: "plan" }
    renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={42}
        workflowGraph={planCurrent}
        defaultExpanded
      />
    )

    expect(screen.getByTestId("workflow-phase-plan")).toHaveAttribute(
      "data-selected",
      "true"
    )
    expect(screen.getByTestId("workflow-phase-summary")).toHaveTextContent(
      "0 / 2 · 1 running"
    )

    fireEvent.click(screen.getByTestId("workflow-phase-tasks"))
    expect(screen.getByTestId("workflow-phase-tasks")).toHaveAttribute(
      "data-selected",
      "true"
    )
    expect(screen.getByTestId("workflow-phase-summary")).toHaveTextContent(
      "Pending"
    )
  })

  it("keeps sticky phase selection across live snapshot updates", () => {
    const planCurrent = { ...skeletonGraph(), current_phase_id: "plan" }
    const { rerender } = renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={42}
        workflowGraph={planCurrent}
        defaultExpanded
      />
    )

    fireEvent.click(screen.getByTestId("workflow-phase-plan"))
    expect(screen.getByTestId("workflow-phase-plan")).toHaveAttribute(
      "data-selected",
      "true"
    )

    rerender(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <SubAgentOverlay
          delegations={[]}
          activities={[]}
          conversationId={42}
          workflowGraph={adaptiveTaskGraph(1)}
          defaultExpanded
        />
      </NextIntlClientProvider>
    )

    expect(screen.getByTestId("workflow-phase-plan")).toHaveAttribute(
      "data-selected",
      "true"
    )
  })

  it("resets phase selection to the latest current after leaving workflow segment", () => {
    const planCurrent = { ...skeletonGraph(), current_phase_id: "plan" }
    const { rerender } = renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={42}
        workflowGraph={planCurrent}
        defaultExpanded
      />
    )

    fireEvent.click(screen.getByTestId("workflow-phase-plan"))
    expect(screen.getByTestId("workflow-phase-plan")).toHaveAttribute(
      "data-selected",
      "true"
    )

    rerender(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <SubAgentOverlay
          delegations={[]}
          activities={[]}
          conversationId={42}
          workflowGraph={adaptiveTaskGraph(1)}
          defaultExpanded
        />
      </NextIntlClientProvider>
    )

    fireEvent.click(screen.getByTestId("workflow-segment-sessions"))
    fireEvent.click(screen.getByTestId("workflow-segment-workflow"))
    expect(screen.getByTestId("workflow-phase-tasks")).toHaveAttribute(
      "data-selected",
      "true"
    )
    expect(screen.getByTestId("workflow-phase-summary")).toHaveTextContent(
      "Task 1 / 1"
    )
  })

  it("caps summary current nodes at two and opens an actionable session", () => {
    const threeCurrent: WorkflowGraphSnapshot = {
      ...skeletonGraph(),
      current_phase_id: "plan",
      current_node_ids: ["current-1", "current-2", "current-3"],
      nodes: [
        node({
          node_id: "current-1",
          phase_id: "plan",
          role: "implementer",
          agent_type: "codex",
          status: "running",
          title: "First current node",
          is_observed: true,
          latest_child_conversation_id: 101,
          round_count: 2,
        }),
        node({
          node_id: "current-2",
          phase_id: "plan",
          role: "reviewer",
          agent_type: "grok",
          status: "running",
          title: "Second current node",
          is_observed: true,
          latest_child_conversation_id: 102,
        }),
        node({
          node_id: "current-3",
          phase_id: "plan",
          role: "reviewer",
          agent_type: "claude",
          status: "running",
          title: "Third current node",
          is_observed: true,
          latest_child_conversation_id: 103,
        }),
      ],
    }

    renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={42}
        workflowGraph={threeCurrent}
        defaultExpanded
      />
    )

    expect(
      screen.getAllByTestId(/^workflow-summary-current-node-/)
    ).toHaveLength(2)
    const firstCurrent = screen.getByTestId(
      "workflow-summary-current-node-current-1"
    )
    expect(firstCurrent).toHaveTextContent("Running")
    expect(firstCurrent).toHaveTextContent("Role: implementer")
    expect(firstCurrent).toHaveTextContent("Agent: codex")
    expect(firstCurrent).toHaveTextContent("Round 2")
    const overflow = screen.getByText("+1")
    expect(overflow.tagName).toBe("SPAN")
    fireEvent.click(firstCurrent)
    expect(openDelegatedChildSession).toHaveBeenCalledWith(
      expect.objectContaining({ childConversationId: 101 })
    )
  })

  it("expands graph and marks estimated nodes non-openable", () => {
    renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={42}
        workflowGraph={skeletonGraph()}
        defaultExpanded
      />
    )
    fireEvent.click(screen.getByTestId("workflow-expand-toggle"))
    expect(screen.getByTestId("workflow-graph-panel")).toBeInTheDocument()

    const estimated = screen.getByTestId("workflow-graph-node-n-est")
    expect(estimated).toHaveAttribute("data-estimated", "true")
    expect(estimated).toHaveAttribute("data-openable", "false")
    expect(estimated).toHaveAttribute("aria-disabled", "true")
    expect(estimated).toHaveAttribute("title", "Estimated — no session yet")
    expect(
      screen.queryByTestId("workflow-graph-node-open-n-est")
    ).not.toBeInTheDocument()

    const observed = screen.getByTestId("workflow-graph-node-n-req")
    expect(observed).toHaveAttribute("data-estimated", "false")
    expect(observed).toHaveAttribute("data-openable", "true")
    expect(
      screen.getByTestId("workflow-graph-node-open-n-req")
    ).toBeInTheDocument()
  })

  it("defaults empty lanes collapsed and non-empty plan expanded with toggle", () => {
    renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={42}
        workflowGraph={skeletonGraph()}
        defaultExpanded
      />
    )
    fireEvent.click(screen.getByTestId("workflow-expand-toggle"))

    expect(screen.getByTestId("workflow-lane-toggle-design")).toHaveAttribute(
      "aria-expanded",
      "false"
    )
    expect(screen.getByTestId("workflow-lane-toggle-plan")).toHaveAttribute(
      "aria-expanded",
      "true"
    )
    expect(screen.getByTestId("workflow-lane-toggle-tasks")).toHaveAttribute(
      "aria-expanded",
      "false"
    )
    expect(screen.getByTestId("workflow-lane-toggle-final")).toHaveAttribute(
      "aria-expanded",
      "false"
    )
    expect(
      within(screen.getByTestId("workflow-graph-lane-design")).getByText(
        "No work units"
      )
    ).toBeInTheDocument()
    expect(screen.getByTestId("workflow-graph-node-n-req")).toBeInTheDocument()

    fireEvent.click(screen.getByTestId("workflow-lane-toggle-plan"))
    expect(screen.getByTestId("workflow-lane-toggle-plan")).toHaveAttribute(
      "aria-expanded",
      "false"
    )
    expect(screen.getByTestId("workflow-graph-lane-plan")).toBeInTheDocument()
    expect(
      screen.queryByTestId("workflow-graph-node-n-req")
    ).not.toBeInTheDocument()
  })

  it("expands a non-dirty empty design lane when nodes appear, then collapses when empty again", () => {
    const emptyDesign = {
      ...skeletonGraph(),
      graph_revision: 10,
    }
    const { rerender } = renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={401}
        workflowGraph={emptyDesign}
        defaultExpanded
      />
    )
    fireEvent.click(screen.getByTestId("workflow-expand-toggle"))
    expect(screen.getByTestId("workflow-lane-toggle-design")).toHaveAttribute(
      "aria-expanded",
      "false"
    )

    const withDesign: WorkflowGraphSnapshot = {
      ...skeletonGraph(),
      graph_revision: 11,
      nodes: [
        ...skeletonGraph().nodes,
        node({
          node_id: "design-author",
          phase_id: "design",
          role: "author",
          title: "Design author",
          status: "running",
          is_observed: true,
          latest_child_conversation_id: 201,
        }),
      ],
    }
    rerender(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <SubAgentOverlay
          delegations={[]}
          activities={[]}
          conversationId={401}
          workflowGraph={withDesign}
          defaultExpanded
        />
      </NextIntlClientProvider>
    )
    expect(screen.getByTestId("workflow-lane-toggle-design")).toHaveAttribute(
      "aria-expanded",
      "true"
    )
    expect(
      screen.getByTestId("workflow-graph-node-design-author")
    ).toBeInTheDocument()

    const emptyAgain: WorkflowGraphSnapshot = {
      ...skeletonGraph(),
      graph_revision: 12,
    }
    rerender(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <SubAgentOverlay
          delegations={[]}
          activities={[]}
          conversationId={401}
          workflowGraph={emptyAgain}
          defaultExpanded
        />
      </NextIntlClientProvider>
    )
    expect(screen.getByTestId("workflow-lane-toggle-design")).toHaveAttribute(
      "aria-expanded",
      "false"
    )
  })

  it("keeps a dirty design lane expanded across empty and non-empty flips", () => {
    const { rerender } = renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={402}
        workflowGraph={{ ...skeletonGraph(), graph_revision: 20 }}
        defaultExpanded
      />
    )
    fireEvent.click(screen.getByTestId("workflow-expand-toggle"))
    fireEvent.click(screen.getByTestId("workflow-lane-toggle-design"))
    expect(screen.getByTestId("workflow-lane-toggle-design")).toHaveAttribute(
      "aria-expanded",
      "true"
    )

    const withDesign: WorkflowGraphSnapshot = {
      ...skeletonGraph(),
      graph_revision: 21,
      nodes: [
        ...skeletonGraph().nodes,
        node({
          node_id: "design-author",
          phase_id: "design",
          role: "author",
          title: "Design author",
          status: "running",
          is_observed: true,
          latest_child_conversation_id: 202,
        }),
      ],
    }
    rerender(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <SubAgentOverlay
          delegations={[]}
          activities={[]}
          conversationId={402}
          workflowGraph={withDesign}
          defaultExpanded
        />
      </NextIntlClientProvider>
    )
    expect(screen.getByTestId("workflow-lane-toggle-design")).toHaveAttribute(
      "aria-expanded",
      "true"
    )

    rerender(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <SubAgentOverlay
          delegations={[]}
          activities={[]}
          conversationId={402}
          workflowGraph={{ ...skeletonGraph(), graph_revision: 22 }}
          defaultExpanded
        />
      </NextIntlClientProvider>
    )
    expect(screen.getByTestId("workflow-lane-toggle-design")).toHaveAttribute(
      "aria-expanded",
      "true"
    )
  })

  it("resets lane expansion defaults after graph panel unmount", () => {
    renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={403}
        workflowGraph={skeletonGraph()}
        defaultExpanded
      />
    )
    fireEvent.click(screen.getByTestId("workflow-expand-toggle"))
    fireEvent.click(screen.getByTestId("workflow-lane-toggle-plan"))
    expect(screen.getByTestId("workflow-lane-toggle-plan")).toHaveAttribute(
      "aria-expanded",
      "false"
    )

    fireEvent.click(screen.getByTestId("workflow-expand-toggle"))
    expect(screen.queryByTestId("workflow-graph-panel")).not.toBeInTheDocument()

    fireEvent.click(screen.getByTestId("workflow-expand-toggle"))
    expect(screen.getByTestId("workflow-lane-toggle-plan")).toHaveAttribute(
      "aria-expanded",
      "true"
    )
    expect(screen.getByTestId("workflow-lane-toggle-design")).toHaveAttribute(
      "aria-expanded",
      "false"
    )
  })

  it("shows run counts on the node card without expanding a detail panel", () => {
    renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={42}
        workflowGraph={skeletonGraph()}
        defaultExpanded
      />
    )
    fireEvent.click(screen.getByTestId("workflow-expand-toggle"))

    const nodeEl = screen.getByTestId("workflow-graph-node-n-req")
    expect(nodeEl).toHaveTextContent("Runs 1")
    expect(nodeEl).toHaveTextContent("Role: reviewer")
    expect(screen.queryByTestId("workflow-node-detail")).not.toBeInTheDocument()
  })

  it("covers the full graph vocabulary in one acceptance fixture", () => {
    renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={55}
        workflowGraph={fullVocabularyGraph()}
        defaultExpanded
      />
    )

    const currentRow = screen.getByTestId(
      "workflow-summary-current-node-task-review-req"
    )
    expect(currentRow).toHaveTextContent("Running")
    expect(currentRow).toHaveTextContent("Role: reviewer")
    expect(currentRow).toHaveTextContent("Agent: grok")

    fireEvent.click(screen.getByTestId("workflow-expand-toggle"))

    const tasksLane = screen.getByTestId("workflow-graph-lane-tasks")
    expect(screen.getByTestId("workflow-lane-toggle-tasks")).toHaveAttribute(
      "aria-expanded",
      "true"
    )

    const implementer = screen.getByTestId("workflow-graph-node-task-impl")
    expect(implementer).toHaveTextContent("Completed")
    expect(implementer).toHaveTextContent("Role: implementer")
    expect(implementer).toHaveTextContent("Agent: codex")
    expect(implementer).toHaveTextContent("Model: gpt-5.2")
    expect(implementer).toHaveTextContent("Effort: high")
    expect(implementer).toHaveTextContent("Runs 2")
    expect(implementer).toHaveTextContent("Replacements 1")
    expect(implementer.querySelector("[data-node-title]")).toHaveClass(
      "line-clamp-2"
    )
    expect(implementer.querySelector("[data-node-title]")).toHaveTextContent(
      "Full vocab implementer"
    )
    // Line 2 is the title row (not role / agent chrome).
    const titleEl = implementer.querySelector("[data-node-title]")
    expect(titleEl).toBeTruthy()
    expect(titleEl?.textContent?.trim().length).toBeGreaterThan(0)
    const ops = screen.getByTestId("workflow-graph-node-ops-task-impl")
    // Lineage sum (elapsed_completed_ms), not only the latest run window.
    expect(ops).toHaveTextContent("7m 30s")
    expect(ops).toHaveTextContent("12 tool uses")
    expect(ops).toHaveTextContent("4 files")
    expect(ops).toHaveTextContent("+10 -2")
    expect(ops.textContent).toMatch(/\|/)
    expect(implementer).toHaveClass("rounded-lg", "border")
    expect(
      screen.getByTestId("workflow-graph-node-open-task-impl")
    ).toBeInTheDocument()
    expect(screen.queryByTestId("workflow-node-detail")).not.toBeInTheDocument()

    expect(
      within(tasksLane).getByTestId("workflow-task-reviewer-count-1")
    ).toHaveTextContent("0 / 1")
    expect(screen.getByTestId("workflow-task-reviewers-1")).toHaveClass(
      "ms-6",
      "border-s"
    )
    expect(
      within(tasksLane).getAllByTestId(/^workflow-task-reviewer-node-/)
    ).toHaveLength(2)

    const optionalReviewer = screen.getByTestId(
      "workflow-graph-node-task-review-opt"
    )
    expect(optionalReviewer).toHaveTextContent("Optional")
    expect(optionalReviewer).toHaveTextContent("Estimated")
    expect(
      screen.queryByTestId("workflow-graph-node-open-task-review-opt")
    ).not.toBeInTheDocument()

    // Free-form risk evidence stays off the card surface.
    expect(screen.queryByText(/security-boundary\.ts/i)).not.toBeInTheDocument()
    expect(
      screen.queryByText(/Touches D:\/private\/project/i)
    ).not.toBeInTheDocument()

    const edges = screen.getByTestId("workflow-graph-edges")
    expect(screen.getByTestId("workflow-dependencies-toggle")).toHaveAttribute(
      "aria-expanded",
      "false"
    )
    expect(
      screen.getByTestId("workflow-dependencies-toggle")
    ).toHaveTextContent("Dependencies (2)")
    fireEvent.click(screen.getByTestId("workflow-dependencies-toggle"))
    expect(screen.getByTestId("workflow-dependencies-toggle")).toHaveAttribute(
      "aria-expanded",
      "true"
    )
    expect(within(edges).getAllByText("Full vocab implementer").length).toBe(2)
    expect(
      within(edges).getByText("Required task reviewer")
    ).toBeInTheDocument()
    expect(
      within(edges).getByText("Optional task reviewer")
    ).toBeInTheDocument()
    expect(screen.getAllByTestId("workflow-dependency-arrow").length).toBe(2)
  })

  it("renders one normal-risk Task row with one reviewer branch", () => {
    renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={43}
        workflowGraph={adaptiveTaskGraph(1, "normal")}
        defaultExpanded
      />
    )
    fireEvent.click(screen.getByTestId("workflow-expand-toggle"))

    const tasksLane = screen.getByTestId("workflow-graph-lane-tasks")
    expect(
      within(tasksLane).getAllByTestId(/^workflow-task-reviewer-node-/)
    ).toHaveLength(1)
    expect(
      within(tasksLane).getByTestId("workflow-task-reviewer-count-1")
    ).toHaveTextContent("1 / 1")
    expect(screen.getByTestId("workflow-graph-node-task-impl")).toHaveClass(
      "h-auto",
      "rounded-lg"
    )
    expect(
      screen
        .getByTestId("workflow-graph-node-task-impl")
        .querySelector("[data-node-title]")
    ).toHaveClass("line-clamp-2")
    expect(screen.getByTestId("workflow-task-reviewers-1")).toHaveClass(
      "ms-6",
      "border-s"
    )
    expect(screen.queryByTestId("workflow-node-detail")).not.toBeInTheDocument()
  })

  it("renders a high-risk reviewer fan-out and updates returned count", () => {
    const graph = adaptiveTaskGraph(1)
    const { rerender } = renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={44}
        workflowGraph={graph}
        defaultExpanded
      />
    )
    fireEvent.click(screen.getByTestId("workflow-expand-toggle"))

    const tasksLane = screen.getByTestId("workflow-graph-lane-tasks")
    expect(
      within(tasksLane).getAllByTestId(/^workflow-task-reviewer-node-/)
    ).toHaveLength(2)
    expect(
      within(tasksLane).getByTestId("workflow-task-reviewer-count-1")
    ).toHaveTextContent("1 / 2")
    expect(screen.getByTestId("workflow-graph-node-task-impl")).toHaveClass(
      "h-auto"
    )
    expect(
      screen
        .getByTestId("workflow-graph-node-task-impl")
        .querySelector("[data-node-title]")
    ).toHaveClass("line-clamp-2")
    expect(screen.getByTestId("workflow-task-reviewers-1")).toHaveClass(
      "ms-6",
      "border-s"
    )
    expect(
      within(tasksLane).getByTestId("workflow-task-reviewer-count-1")
    ).toHaveTextContent("1 / 2")

    rerender(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <SubAgentOverlay
          delegations={[]}
          activities={[]}
          conversationId={44}
          workflowGraph={adaptiveTaskGraph(2)}
          defaultExpanded
        />
      </NextIntlClientProvider>
    )
    expect(
      screen.getByTestId("workflow-task-reviewer-count-1")
    ).toHaveTextContent("2 / 2")
  })

  it("does not surface free-form risk evidence paths on node cards", () => {
    renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={45}
        workflowGraph={adaptiveTaskGraph(1)}
        defaultExpanded
      />
    )
    fireEvent.click(screen.getByTestId("workflow-expand-toggle"))

    expect(screen.queryByTestId("workflow-node-detail")).not.toBeInTheDocument()
    expect(screen.queryByText(/security-boundary\.ts/i)).not.toBeInTheDocument()
    expect(
      screen.queryByText(/Touches D:\/private\/project/i)
    ).not.toBeInTheDocument()
  })

  it("keeps plan and Task cohorts ordered, keyboard-accessible, and contained", async () => {
    const user = userEvent.setup()
    renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={46}
        workflowGraph={adaptiveTaskGraph(1)}
        defaultExpanded
      />
    )
    fireEvent.click(screen.getByTestId("workflow-expand-toggle"))

    const planLane = screen.getByTestId("workflow-graph-lane-plan")
    const planNodes = within(planLane).getAllByTestId(/^workflow-graph-node-/)
    expect(planNodes.map((nodeEl) => nodeEl.dataset.testid)).toEqual([
      "workflow-graph-node-plan-author",
      "workflow-graph-node-plan-review-grok",
    ])

    const tasksLane = screen.getByTestId("workflow-graph-lane-tasks")
    expect(
      within(tasksLane).getAllByTestId(/^workflow-task-reviewer-node-/)
    ).toHaveLength(2)
    expect(
      within(tasksLane).getByTestId("workflow-task-reviewer-count-1")
    ).toHaveTextContent("1 / 2")
    expect(
      within(tasksLane).getByTestId("workflow-task-reviewers-1")
    ).toHaveClass("ms-6", "border-s")

    const openReviewer = screen.getByTestId(
      "workflow-graph-node-open-task-review-codex"
    )
    openReviewer.focus()
    await user.keyboard("{Enter}")
    expect(openDelegatedChildSession).toHaveBeenCalledWith(
      expect.objectContaining({ childConversationId: 93 })
    )
  })

  it("collapses dependencies by default and shows title chips when expanded", () => {
    const withUnknownEdge: WorkflowGraphSnapshot = {
      ...skeletonGraph(),
      edges: [
        { from: "n-est", to: "n-req" },
        { from: "n-req", to: "unknown-endpoint" },
      ],
    }
    renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={47}
        workflowGraph={withUnknownEdge}
        defaultExpanded
      />
    )
    fireEvent.click(screen.getByTestId("workflow-expand-toggle"))

    const edges = screen.getByTestId("workflow-graph-edges")
    expect(screen.getByTestId("workflow-dependencies-toggle")).toHaveAttribute(
      "aria-expanded",
      "false"
    )
    expect(
      screen.getByTestId("workflow-dependencies-toggle")
    ).toHaveTextContent("Dependencies (2)")
    expect(within(edges).queryByText("Plan reviewer")).not.toBeInTheDocument()
    expect(screen.queryByText(/n-est → n-req/)).not.toBeInTheDocument()
    expect(
      screen.queryByTestId("workflow-dependency-arrow")
    ).not.toBeInTheDocument()

    fireEvent.click(screen.getByTestId("workflow-dependencies-toggle"))
    expect(screen.getByTestId("workflow-dependencies-toggle")).toHaveAttribute(
      "aria-expanded",
      "true"
    )
    expect(within(edges).getByText("Plan reviewer")).toBeInTheDocument()
    expect(
      within(edges).getAllByText("Required reviewer").length
    ).toBeGreaterThanOrEqual(1)
    expect(within(edges).getByText("unknown-endpoint")).toBeInTheDocument()
    expect(screen.queryByText(/n-est → n-req/)).not.toBeInTheDocument()

    for (const arrow of screen.getAllByTestId("workflow-dependency-arrow")) {
      expect(arrow).toHaveClass("rtl:rotate-180")
    }
  })

  it("opens observed sessions from the eye control and never mounts node detail", () => {
    renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={48}
        workflowGraph={skeletonGraph()}
        defaultExpanded
      />
    )
    fireEvent.click(screen.getByTestId("workflow-expand-toggle"))

    expect(screen.queryByTestId("workflow-node-detail")).not.toBeInTheDocument()
    fireEvent.click(screen.getByTestId("workflow-graph-node-open-n-req"))
    expect(openDelegatedChildSession).toHaveBeenCalledWith(
      expect.objectContaining({ childConversationId: 77 })
    )
    expect(screen.queryByTestId("workflow-node-detail")).not.toBeInTheDocument()

    fireEvent.click(screen.getByTestId("workflow-lane-toggle-plan"))
    expect(
      screen.queryByTestId("workflow-graph-node-n-req")
    ).not.toBeInTheDocument()
  })

  it("holds overlay interest while open and expanded interest only for the full graph", () => {
    const releaseOverlay = vi.fn()
    const releaseExpanded = vi.fn()
    const activateOverlay = vi
      .spyOn(useWorkflowGraphStore.getState(), "activateOverlayInterest")
      .mockReturnValue(releaseOverlay)
    const activateExpanded = vi
      .spyOn(useWorkflowGraphStore.getState(), "activateConversation")
      .mockReturnValue(releaseExpanded)

    renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={42}
        workflowGraph={skeletonGraph()}
        defaultExpanded
      />
    )
    expect(activateOverlay).toHaveBeenCalledTimes(1)
    expect(activateOverlay).toHaveBeenCalledWith(42)
    expect(activateExpanded).not.toHaveBeenCalled()

    fireEvent.click(screen.getByTestId("workflow-segment-sessions"))
    expect(activateOverlay).toHaveBeenCalledTimes(1)
    expect(activateExpanded).not.toHaveBeenCalled()
    fireEvent.click(screen.getByTestId("workflow-segment-workflow"))
    fireEvent.click(screen.getByTestId("workflow-expand-toggle"))
    expect(activateExpanded).toHaveBeenCalledTimes(1)
    expect(activateExpanded).toHaveBeenCalledWith(42)
    fireEvent.click(
      screen.getByRole("button", { name: "Collapse workflow graph" })
    )
    expect(releaseExpanded).toHaveBeenCalledTimes(1)
    expect(releaseOverlay).not.toHaveBeenCalled()

    fireEvent.click(screen.getByRole("button", { name: "Collapse sub-agents" }))
    expect(releaseOverlay).not.toHaveBeenCalled()
  })

  it("chip-collapsed active overlays retain overlay interest", () => {
    const releaseOverlay = vi.fn()
    const activateOverlay = vi
      .spyOn(useWorkflowGraphStore.getState(), "activateOverlayInterest")
      .mockReturnValue(releaseOverlay)
    const activateExpanded = vi.spyOn(
      useWorkflowGraphStore.getState(),
      "activateConversation"
    )
    const { unmount } = renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={42}
        workflowGraph={skeletonGraph()}
        defaultExpanded={false}
      />
    )
    expect(activateOverlay).toHaveBeenCalledOnce()
    expect(activateOverlay).toHaveBeenCalledWith(42)
    expect(activateExpanded).not.toHaveBeenCalled()
    unmount()
    expect(releaseOverlay).toHaveBeenCalledOnce()
  })

  it.each([0, -1])(
    "non-positive id %s acquires no interest",
    (conversationId) => {
      const activateOverlay = vi.spyOn(
        useWorkflowGraphStore.getState(),
        "activateOverlayInterest"
      )
      const activateExpanded = vi.spyOn(
        useWorkflowGraphStore.getState(),
        "activateConversation"
      )
      renderWithIntl(
        <SubAgentOverlay
          delegations={[]}
          activities={[]}
          conversationId={conversationId}
          workflowGraph={skeletonGraph()}
          defaultExpanded
        />
      )
      expect(activateOverlay).not.toHaveBeenCalled()
      expect(activateExpanded).not.toHaveBeenCalled()
      fireEvent.click(screen.getByTestId("workflow-expand-toggle"))
      expect(activateOverlay).not.toHaveBeenCalled()
      expect(activateExpanded).not.toHaveBeenCalled()
    }
  )

  it("switching segments and collapsing releases only expanded interest", () => {
    const releaseOverlay = vi.fn()
    const releaseExpanded1 = vi.fn()
    const releaseExpanded2 = vi.fn()
    const activateOverlay = vi
      .spyOn(useWorkflowGraphStore.getState(), "activateOverlayInterest")
      .mockReturnValue(releaseOverlay)
    const activateExpanded = vi
      .spyOn(useWorkflowGraphStore.getState(), "activateConversation")
      .mockReturnValueOnce(releaseExpanded1)
      .mockReturnValueOnce(releaseExpanded2)

    renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={42}
        workflowGraph={skeletonGraph()}
        defaultExpanded
      />
    )
    expect(activateOverlay).toHaveBeenCalledTimes(1)
    fireEvent.click(screen.getByTestId("workflow-expand-toggle"))
    expect(activateExpanded).toHaveBeenCalledTimes(1)

    fireEvent.click(screen.getByTestId("workflow-segment-sessions"))
    expect(releaseExpanded1).toHaveBeenCalledTimes(1)
    expect(releaseOverlay).not.toHaveBeenCalled()

    fireEvent.click(screen.getByTestId("workflow-segment-workflow"))
    expect(activateOverlay).toHaveBeenCalledTimes(1)
    expect(releaseOverlay).not.toHaveBeenCalled()
    expect(activateExpanded).toHaveBeenCalledTimes(2)
    expect(activateExpanded.mock.calls.map((call) => call[0])).toEqual([42, 42])

    fireEvent.click(screen.getByRole("button", { name: "Collapse sub-agents" }))
    expect(releaseExpanded1).toHaveBeenCalledTimes(1)
    expect(releaseExpanded2).toHaveBeenCalledTimes(1)
    expect(releaseOverlay).not.toHaveBeenCalled()
  })

  it("becoming inactive releases overlay and expanded interest", () => {
    const releaseOverlay = vi.fn()
    const releaseExpanded = vi.fn()
    const activateOverlay = vi
      .spyOn(useWorkflowGraphStore.getState(), "activateOverlayInterest")
      .mockReturnValue(releaseOverlay)
    const activateExpanded = vi
      .spyOn(useWorkflowGraphStore.getState(), "activateConversation")
      .mockReturnValue(releaseExpanded)

    const { rerender } = renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={42}
        workflowGraph={skeletonGraph()}
        defaultExpanded
        isActive
      />
    )
    fireEvent.click(screen.getByTestId("workflow-expand-toggle"))
    expect(activateOverlay).toHaveBeenCalledWith(42)
    expect(activateExpanded).toHaveBeenCalledWith(42)

    rerender(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <SubAgentOverlay
          delegations={[]}
          activities={[]}
          conversationId={42}
          workflowGraph={skeletonGraph()}
          defaultExpanded
          isActive={false}
        />
      </NextIntlClientProvider>
    )

    expect(releaseOverlay).toHaveBeenCalledOnce()
    expect(releaseExpanded).toHaveBeenCalledOnce()
  })

  it("detail updates reseed without reinstalling active interest", () => {
    const releaseOverlay = vi.fn()
    const releaseExpanded = vi.fn()
    const activateOverlay = vi
      .spyOn(useWorkflowGraphStore.getState(), "activateOverlayInterest")
      .mockReturnValue(releaseOverlay)
    const activateExpanded = vi
      .spyOn(useWorkflowGraphStore.getState(), "activateConversation")
      .mockReturnValue(releaseExpanded)

    const { rerender } = renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={42}
        workflowGraph={skeletonGraph()}
        defaultExpanded
      />
    )
    expect(activateOverlay).toHaveBeenCalledTimes(1)
    fireEvent.click(screen.getByTestId("workflow-expand-toggle"))
    expect(activateExpanded).toHaveBeenCalledTimes(1)

    const revision2: WorkflowGraphSnapshot = {
      ...skeletonGraph(),
      graph_revision: 2,
    }
    rerender(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <SubAgentOverlay
          delegations={[]}
          activities={[]}
          conversationId={42}
          workflowGraph={revision2}
          defaultExpanded
        />
      </NextIntlClientProvider>
    )
    expect(
      useWorkflowGraphStore.getState().getSnapshot(42)?.graph_revision
    ).toBe(2)
    expect(activateOverlay).toHaveBeenCalledTimes(1)
    expect(activateExpanded).toHaveBeenCalledTimes(1)
    expect(releaseOverlay).not.toHaveBeenCalled()
    expect(releaseExpanded).not.toHaveBeenCalled()
  })

  it("changing conversation id releases the old lease and activates the new id", () => {
    const releaseOverlay81 = vi.fn()
    const releaseOverlay82 = vi.fn()
    const releaseExpanded81 = vi.fn()
    const releaseExpanded82 = vi.fn()
    const activateOverlay = vi
      .spyOn(useWorkflowGraphStore.getState(), "activateOverlayInterest")
      .mockReturnValueOnce(releaseOverlay81)
      .mockReturnValueOnce(releaseOverlay82)
    const activateExpanded = vi
      .spyOn(useWorkflowGraphStore.getState(), "activateConversation")
      .mockReturnValueOnce(releaseExpanded81)
      .mockReturnValueOnce(releaseExpanded82)

    const { rerender, unmount } = renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={81}
        workflowGraph={skeletonGraph()}
        defaultExpanded
      />
    )
    fireEvent.click(screen.getByTestId("workflow-expand-toggle"))
    expect(activateOverlay.mock.calls.map((call) => call[0])).toEqual([81])
    expect(activateExpanded.mock.calls.map((call) => call[0])).toEqual([81])

    rerender(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <SubAgentOverlay
          delegations={[]}
          activities={[]}
          conversationId={82}
          workflowGraph={skeletonGraph()}
          defaultExpanded
        />
      </NextIntlClientProvider>
    )
    expect(releaseOverlay81).toHaveBeenCalledTimes(1)
    expect(releaseOverlay82).not.toHaveBeenCalled()
    expect(releaseExpanded81).toHaveBeenCalledTimes(1)
    expect(releaseExpanded82).not.toHaveBeenCalled()
    expect(activateOverlay.mock.calls.map((call) => call[0])).toEqual([81, 82])
    expect(activateExpanded.mock.calls.map((call) => call[0])).toEqual([81, 82])

    unmount()
    expect(releaseOverlay82).toHaveBeenCalledTimes(1)
    expect(releaseExpanded82).toHaveBeenCalledTimes(1)
  })

  it("unmount releases overlay and expanded workflow leases", () => {
    const releaseOverlay = vi.fn()
    const releaseExpanded = vi.fn()
    const activateOverlay = vi
      .spyOn(useWorkflowGraphStore.getState(), "activateOverlayInterest")
      .mockReturnValue(releaseOverlay)
    const activateExpanded = vi
      .spyOn(useWorkflowGraphStore.getState(), "activateConversation")
      .mockReturnValue(releaseExpanded)

    const { unmount } = renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={42}
        workflowGraph={skeletonGraph()}
        defaultExpanded
      />
    )
    expect(activateOverlay).toHaveBeenCalledTimes(1)
    expect(activateOverlay).toHaveBeenCalledWith(42)
    fireEvent.click(screen.getByTestId("workflow-expand-toggle"))
    expect(activateExpanded).toHaveBeenCalledTimes(1)
    expect(activateExpanded).toHaveBeenCalledWith(42)

    unmount()
    expect(releaseOverlay).toHaveBeenCalledTimes(1)
    expect(releaseExpanded).toHaveBeenCalledTimes(1)
  })
})
