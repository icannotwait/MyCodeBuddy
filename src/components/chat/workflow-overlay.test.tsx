import {
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
import enMessages from "@/i18n/messages/en.json"
import { getWorkflowGraphSnapshot } from "@/lib/api"
import { openDelegatedChildSession } from "@/lib/open-delegated-child-session"
import type { WorkflowGraphSnapshot, WorkflowNodeSnapshot } from "@/lib/types"
import {
  __resetWorkflowGraphStoreForTests,
  useWorkflowGraphStore,
} from "@/lib/workflow-graph-store"

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

function renderWithIntl(ui: React.ReactElement) {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      {ui}
    </NextIntlClientProvider>
  )
}

beforeEach(() => {
  __resetWorkflowGraphStoreForTests()
  vi.mocked(openDelegatedChildSession).mockClear()
  vi.mocked(getWorkflowGraphSnapshot).mockReset()
  vi.mocked(getWorkflowGraphSnapshot).mockResolvedValue(null)
})

afterEach(() => {
  vi.restoreAllMocks()
})

describe("SubAgentOverlay A13 workflow mount", () => {
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
    expect(releaseOverlay).toHaveBeenCalledTimes(1)
  })

  it("chip-collapsed overlays acquire no interest", () => {
    const activateOverlay = vi.spyOn(
      useWorkflowGraphStore.getState(),
      "activateOverlayInterest"
    )
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
    expect(activateOverlay).not.toHaveBeenCalled()
    expect(activateExpanded).not.toHaveBeenCalled()
    unmount()
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

  it("switching segments and collapsing the overlay releases the active lease", () => {
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
    expect(releaseOverlay).toHaveBeenCalledTimes(1)
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
