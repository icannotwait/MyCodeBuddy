import { fireEvent, render, screen } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"

import { SubAgentOverlay } from "./sub-agent-overlay"
import enMessages from "@/i18n/messages/en.json"
import { openDelegatedChildSession } from "@/lib/open-delegated-child-session"
import type { WorkflowGraphSnapshot, WorkflowNodeSnapshot } from "@/lib/types"
import { __resetWorkflowGraphStoreForTests } from "@/lib/workflow-graph-store"

vi.mock("@/hooks/use-delegated-sub-session", () => ({
  useDelegatedSubSession: vi.fn(() => null),
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
})

describe("SubAgentOverlay A13 workflow mount", () => {
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

  it("shows B11 required gate counts on plan phase", () => {
    renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={42}
        workflowGraph={skeletonGraph()}
        defaultExpanded
      />
    )
    const gate = screen.getByTestId("workflow-phase-gate-plan")
    // required_count=2, returned=0 — optional reviewer must not inflate required
    expect(gate).toHaveTextContent("0 / 2")
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
    expect(estimated).toBeDisabled()

    const observed = screen.getByTestId("workflow-graph-node-n-req")
    expect(observed).toHaveAttribute("data-estimated", "false")
    expect(observed).toHaveAttribute("data-openable", "true")
    expect(observed).not.toBeDisabled()
  })

  it("exposes B12 fields in node detail", () => {
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
    fireEvent.click(screen.getByTestId("workflow-graph-node-n-req"))
    expect(screen.getByTestId("workflow-node-detail")).toBeInTheDocument()
    expect(screen.getByTestId("workflow-node-run-count")).toHaveTextContent("1")
    expect(
      screen.getByTestId("workflow-node-replacement-count")
    ).toHaveTextContent("0")
    expect(screen.getByTestId("workflow-node-b12")).toBeInTheDocument()
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

    const row = screen.getByTestId("workflow-graph-row-tasks-1")
    expect(row).toHaveAttribute("data-reviewer-count", "1")
    expect(screen.getAllByTestId(/^workflow-task-reviewer-node-/)).toHaveLength(
      1
    )
    expect(
      screen.getByTestId("workflow-task-reviewer-count-1")
    ).toHaveTextContent("1 / 1")
    fireEvent.click(screen.getByTestId("workflow-graph-node-task-impl"))
    expect(screen.getByTestId("workflow-node-risk-level")).toHaveTextContent(
      "Normal risk"
    )
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

    const row = screen.getByTestId("workflow-graph-row-tasks-1")
    expect(row).toHaveAttribute("data-reviewer-count", "2")
    expect(screen.getAllByTestId(/^workflow-task-reviewer-node-/)).toHaveLength(
      2
    )
    expect(
      screen.getByTestId("workflow-task-reviewer-count-1")
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

  it("shows localized risk metadata without free-form evidence paths", () => {
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
    fireEvent.click(screen.getByTestId("workflow-graph-node-task-impl"))

    expect(screen.getByTestId("workflow-node-risk-level")).toHaveTextContent(
      "High risk"
    )
    expect(screen.getByTestId("workflow-node-risk-reasons")).toHaveTextContent(
      "Security or trust boundary"
    )
    expect(screen.getByTestId("workflow-node-risk-reasons")).toHaveTextContent(
      "Shared interface"
    )
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

    const planRow = screen.getByTestId("workflow-graph-row-plan")
    const planNodes = planRow.querySelectorAll("button")
    expect(planNodes[0]).toHaveAttribute(
      "data-testid",
      "workflow-graph-node-plan-author"
    )
    expect(planNodes[1]).toHaveAttribute(
      "data-testid",
      "workflow-graph-node-plan-review-grok"
    )

    const taskRow = screen.getByTestId("workflow-graph-row-tasks-1")
    expect(taskRow).toHaveClass("min-w-0", "overflow-hidden")
    expect(screen.getByTestId("workflow-task-reviewers-1")).toHaveClass(
      "min-w-0"
    )

    const reviewer = screen.getByTestId("workflow-graph-node-task-review-codex")
    reviewer.focus()
    await user.keyboard("{Enter}")
    expect(openDelegatedChildSession).toHaveBeenCalledWith(
      expect.objectContaining({ childConversationId: 93 })
    )
  })
})
