import { act, fireEvent, render, screen, waitFor } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"

import {
  groupDelegationSourcesForOverlay,
  SubAgentOverlay,
} from "./sub-agent-overlay"
import enMessages from "@/i18n/messages/en.json"
import type { DelegationBinding } from "@/contexts/delegation-context"
import type { DelegationCardSource } from "@/hooks/use-delegation-card-model"
import { delegationRunSnapshotCache } from "@/lib/delegation-run-snapshot"
import { continueArchivedWorkflowInSimple } from "@/lib/api"
import { getActiveBackendCacheKey } from "@/lib/transport"
import type {
  DelegationRunSnapshot,
  SimpleSuccessorResult,
  WorkflowGraphSnapshot,
} from "@/lib/types"

// The rows resolve their model from `useDelegatedSubSession` (live binding) and
// the connections store (child pending-permission). Stub both — the same
// contexts DelegatedSubThread's own test stubs.
vi.mock("@/hooks/use-delegated-sub-session", () => ({
  useDelegatedSubSession: vi.fn(),
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

// Open path goes through openDelegatedChildSession (main tab). Stub the helper
// so overlay tests stay unit-scoped.
const { openDelegatedChildSession } = vi.hoisted(() => ({
  openDelegatedChildSession: vi.fn(async () => true),
}))
vi.mock("@/lib/open-delegated-child-session", () => ({
  openDelegatedChildSession: (...args: unknown[]) =>
    openDelegatedChildSession(...args),
}))

vi.mock("@/components/ai-elements/link-safety", () => ({
  useOpenLinkOrFile: () => vi.fn(async () => {}),
}))

vi.mock("@/lib/api", async () => {
  const actual = await vi.importActual<typeof import("@/lib/api")>("@/lib/api")
  return {
    ...actual,
    continueArchivedWorkflowInSimple: vi.fn(),
  }
})

const { useDelegatedSubSession } =
  await import("@/hooks/use-delegated-sub-session")
const mockedHook = vi.mocked(useDelegatedSubSession)

/** Per-parentToolUseId binding map the mocked hook reads from. */
let bindings: Record<string, DelegationBinding | undefined> = {}

function bindingOf(overrides: Partial<DelegationBinding>): DelegationBinding {
  return {
    parentConnectionId: "p1",
    parentToolUseId: "pt-1",
    childConnectionId: "c1",
    childConversationId: 99,
    agentType: "codex",
    status: "running",
    task: null,
    taskId: "task-1",
    startedAt: "2026-07-19T00:00:00.000Z",
    runtimeStats: {
      started_at: "2026-07-19T00:00:00.000Z",
      tool_call_count: 0,
      edit_tool_call_count: 0,
      touched_files: [],
      touched_files_truncated: false,
      line_counts_complete: false,
    },
    ...overrides,
  }
}

function renderWithIntl(ui: React.ReactElement) {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      {ui}
    </NextIntlClientProvider>
  )
}

function source(
  parentToolUseId: string,
  args: Record<string, unknown>
): DelegationCardSource {
  return { parentToolUseId, input: JSON.stringify(args) }
}

function snapshotFor(
  taskId: string,
  generation: number,
  childConversationId: number
): DelegationRunSnapshot {
  return {
    task_id: taskId,
    root_task_id: "run-1",
    previous_task_id: generation > 1 ? "run-1" : null,
    generation,
    parent_tool_use_id: `pt-${generation}`,
    child_conversation_id: childConversationId,
    agent_type: "codex",
    profile_id: null,
    task_preview: `Task ${generation}`,
    status: "completed",
    error_code: null,
    started_at: "2026-07-21T10:00:00.000Z",
    finished_at: "2026-07-21T10:01:00.000Z",
    runtime_stats: null,
    card_summary: null,
    child_turn_anchor: null,
    replaced_task_id: null,
    replacement_reason: null,
  }
}

function snapshotCacheKey(
  parentConversationId: number,
  taskId: string
): string {
  return `${getActiveBackendCacheKey()}\0${parentConversationId}\0${taskId}`
}

function archivedSnapshot(): WorkflowGraphSnapshot {
  return {
    schema_version: 1,
    workflow_id: "archived-workflow",
    workflow_kind: "brainstorm_to_delivery",
    manifest_revision: 3,
    graph_revision: 7,
    manifest_state: "approved",
    compatibility: "manifest",
    overall_state: "approved",
    archived: {
      source_conversation_id: 42,
      plan_rel_path: "docs/plan.md",
      successor_conversation_id: null,
      can_create_simple_successor: true,
    },
    projection_warning_codes: [],
    current_phase_id: null,
    current_node_ids: [],
    phases: [],
    nodes: [],
    edges: [],
    gates: [],
  }
}

function deferred<T>(): {
  promise: Promise<T>
  resolve: (value: T) => void
} {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((accept) => {
    resolve = accept
  })
  return { promise, resolve }
}

describe("SubAgentOverlay", () => {
  beforeEach(() => {
    localStorage.clear()
    delegationRunSnapshotCache.reset()
    bindings = {}
    openDelegatedChildSession.mockClear()
    vi.mocked(continueArchivedWorkflowInSimple).mockReset()
    mockedHook.mockReset()
    mockedHook.mockImplementation((id: string) => ({
      binding: bindings[id],
      detail: null,
      loading: false,
      error: null,
    }))
  })

  it("renders nothing when there are no delegations", () => {
    const { container } = renderWithIntl(
      <SubAgentOverlay delegations={[]} overlayKey="k-empty" />
    )
    expect(container.firstChild).toBeNull()
  })

  it("deduplicates an archived successor action with one stable request token", async () => {
    const pending = deferred<SimpleSuccessorResult>()
    const onOpenRootConversation = vi.fn()
    vi.mocked(continueArchivedWorkflowInSimple).mockReturnValue(pending.promise)
    renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        workflowGraph={archivedSnapshot()}
        onOpenRootConversation={onOpenRootConversation}
      />
    )

    const action = screen.getByRole("button", { name: "Continue in Simple" })
    fireEvent.click(action)
    fireEvent.click(action)

    expect(continueArchivedWorkflowInSimple).toHaveBeenCalledTimes(1)
    const [sourceConversationId, requestToken] = vi.mocked(
      continueArchivedWorkflowInSimple
    ).mock.calls[0]
    expect(sourceConversationId).toBe(42)
    expect(requestToken).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i
    )
    expect(action).toBeDisabled()

    await act(async () => {
      pending.resolve({
        successor_conversation_id: 84,
        created: true,
        plan_rel_path: "docs/plan.md",
        progress_rel_path: ".superpowers/sdd/84/progress.md",
        bootstrap_prompt: "Continue the archived workflow.",
      })
      await pending.promise
    })
    expect(onOpenRootConversation).toHaveBeenCalledTimes(1)
    expect(onOpenRootConversation).toHaveBeenCalledWith(84)
  })

  it("deduplicates navigation while an existing Simple successor is opening", async () => {
    const navigation = deferred<void>()
    const onOpenRootConversation = vi.fn(() => navigation.promise)
    const snapshot = archivedSnapshot()
    snapshot.archived = {
      ...snapshot.archived!,
      successor_conversation_id: 84,
      can_create_simple_successor: false,
    }
    renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        workflowGraph={snapshot}
        onOpenRootConversation={onOpenRootConversation}
      />
    )

    const action = screen.getByRole("button", {
      name: "Open Simple successor",
    })
    fireEvent.click(action)
    fireEvent.click(action)

    expect(onOpenRootConversation).toHaveBeenCalledTimes(1)
    expect(onOpenRootConversation).toHaveBeenCalledWith(84)
    expect(continueArchivedWorkflowInSimple).not.toHaveBeenCalled()
    expect(action).toBeDisabled()

    await act(async () => {
      navigation.resolve()
      await navigation.promise
    })
  })

  it("keeps archived source history visible when successor creation fails", async () => {
    vi.mocked(continueArchivedWorkflowInSimple).mockRejectedValue(
      new Error("Unable to create successor")
    )
    renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        workflowGraph={archivedSnapshot()}
        onOpenRootConversation={vi.fn()}
      />
    )

    fireEvent.click(
      screen.getByRole("button", { name: "Continue in Simple" })
    )

    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent(
        "Unable to create successor"
      )
    )
    expect(screen.getByTestId("workflow-archived-banner")).toBeVisible()
    expect(screen.getAllByText("Workflow").length).toBeGreaterThan(0)
    expect(
      screen.getByRole("button", { name: "Continue in Simple" })
    ).toBeEnabled()
  })

  it("expands by default and lists every sub-agent", () => {
    const delegations = [
      source("pt-1", { agent_type: "codex", task: "Investigate flaky test" }),
      source("pt-2", { agent_type: "claude_code", task: "Write the fix" }),
    ]
    renderWithIntl(
      <SubAgentOverlay delegations={delegations} overlayKey="k-1" />
    )
    // Header title + both rows (one per delegation).
    expect(screen.getByText("Sub-agents")).toBeInTheDocument()
    expect(screen.getAllByTestId("sub-agent-row")).toHaveLength(2)
    expect(screen.getByText("Investigate flaky test")).toBeInTheDocument()
    expect(screen.getByText("Write the fix")).toBeInTheDocument()
  })

  it("groups reusable runs by child conversation and keeps replacements separate", () => {
    const reusableMeta = (taskId: string, generation: number) => ({
      "codeg.delegation": {
        status: "completed",
        task_id: taskId,
        child_conversation_id: 77,
        generation,
      },
    })
    const replacementMeta = {
      "codeg.delegation": {
        status: "completed",
        task_id: "replacement-1",
        child_conversation_id: 88,
        generation: 1,
        replaced_task_id: "run-2",
      },
    }
    const delegations: DelegationCardSource[] = [
      {
        ...source("pt-1", {
          agent_type: "codex",
          task: "Review the change",
        }),
        meta: reusableMeta("run-1", 1),
      },
      {
        ...source("pt-2", {
          agent_type: "codex",
          task: "Re-review the revision",
        }),
        meta: reusableMeta("run-2", 2),
      },
      {
        ...source("pt-3", {
          agent_type: "codex",
          task: "Replacement review",
        }),
        meta: replacementMeta,
      },
    ]

    renderWithIntl(
      <SubAgentOverlay delegations={delegations} overlayKey="k-grouped-runs" />
    )

    expect(screen.getByTestId("sub-agent-overlay-group-77")).toHaveTextContent(
      "2 runs"
    )
    expect(screen.getByTestId("sub-agent-overlay-group-88")).toHaveTextContent(
      "Replacement"
    )
    expect(screen.getAllByTestId("sub-agent-row")).toHaveLength(2)
  })

  it("groups three cold historical rounds and selects the newest generation", () => {
    const continuationOutput =
      "Continuation running in the existing child session. " +
      "Call get_delegation_status with the returned task_id to collect the result."
    const delegations: DelegationCardSource[] = [1, 2, 3].map((generation) => ({
      ...source(`pt-${generation}`, {
        agent_type: "grok",
        task: `Revision ${generation}`,
      }),
      output:
        generation === 1
          ? "Delegation successful. task_id=run-1."
          : continuationOutput,
      meta: {
        "codeg.delegation": {
          status: "completed",
          task_id: `run-${generation}`,
          child_conversation_id: 77,
          generation,
          synthetic_historical: true,
        },
      },
    }))

    const groups = groupDelegationSourcesForOverlay(delegations)

    expect(groups).toHaveLength(1)
    expect(groups[0]).toMatchObject({
      childConversationId: 77,
      latestGeneration: 3,
      latestIndex: 2,
      runCount: 3,
    })
    expect(groups[0].latestSource.parentToolUseId).toBe("pt-3")
  })

  it("binds to a newer live run before its generation metadata arrives", () => {
    const groups = groupDelegationSourcesForOverlay([
      {
        ...source("pt-2", {
          agent_type: "codex",
          task: "Historical run",
          work_unit_key: "unit-a",
        }),
        parentConversationId: 10,
        meta: {
          "codeg.delegation": {
            status: "completed",
            task_id: "run-2",
            child_conversation_id: 77,
            generation: 2,
          },
        },
      },
      {
        ...source("pt-live", {
          agent_type: "codex",
          task: "Live continuation",
          work_unit_key: "unit-a",
          task_id: "run-2",
        }),
        parentConversationId: 10,
        output: JSON.stringify({ status: "running", task_id: "run-3" }),
      },
    ])

    expect(groups).toHaveLength(1)
    expect(groups[0]).toMatchObject({
      latestIndex: 1,
      latestGeneration: null,
    })
    expect(groups[0].latestSource.parentToolUseId).toBe("pt-live")
  })

  it("groups different child sessions when their explicit work key matches", () => {
    const groups = groupDelegationSourcesForOverlay([
      {
        ...source("pt-1", {
          agent_type: "codex",
          task: "Initial run",
          work_unit_key: "unit-a",
        }),
        parentConversationId: 10,
        output: JSON.stringify({
          status: "completed",
          task_id: "run-1",
          child_conversation_id: 77,
        }),
      },
      {
        ...source("pt-2", {
          agent_type: "codex",
          task: "Replacement run",
          work_unit_key: "unit-a",
        }),
        parentConversationId: 10,
        output: JSON.stringify({
          status: "running",
          task_id: "run-2",
          child_conversation_id: 88,
        }),
      },
    ])

    expect(groups).toHaveLength(1)
    expect(groups[0].key).toBe("unit-a")
    expect(groups[0].runCount).toBe(2)
    expect(groups[0].sources).toHaveLength(2)
  })

  it("keeps conflicting explicit work keys separate on the same child", () => {
    const groups = groupDelegationSourcesForOverlay([
      {
        ...source("pt-1", {
          agent_type: "codex",
          task: "Parallel A",
          work_unit_key: "unit-a",
        }),
        parentConversationId: 10,
        output: JSON.stringify({
          status: "running",
          task_id: "run-a",
          child_conversation_id: 77,
        }),
      },
      {
        ...source("pt-2", {
          agent_type: "codex",
          task: "Parallel B",
          work_unit_key: "unit-b",
        }),
        parentConversationId: 10,
        output: JSON.stringify({
          status: "running",
          task_id: "run-b",
          child_conversation_id: 77,
        }),
      },
    ])

    expect(groups.map((group) => group.key)).toEqual(["unit-a", "unit-b"])
  })

  it("renders aggregated sticky runtime for a grouped work unit", () => {
    const startedAt = "2026-07-27T00:00:00.000Z"
    const continuedAt = "2026-07-27T00:05:00.000Z"
    const runtimeStats = (start: string, count: number) => ({
      started_at: start,
      tool_call_count: count,
      edit_tool_call_count: 0,
      touched_files: [],
      touched_files_truncated: false,
      line_counts_complete: false,
    })
    const delegations: DelegationCardSource[] = [
      {
        ...source("pt-1", {
          agent_type: "codex",
          task: "Initial run",
          work_unit_key: "unit-a",
        }),
        parentConversationId: 10,
        meta: {
          "codeg.delegation": {
            status: "completed",
            task_id: "run-1",
            child_conversation_id: 77,
            generation: 1,
            started_at: startedAt,
            finished_at: continuedAt,
            runtime_stats: {
              ...runtimeStats(startedAt, 5),
              finished_at: continuedAt,
            },
          },
        },
      },
      {
        ...source("pt-2", {
          agent_type: "codex",
          task: "Continued run",
          work_unit_key: "unit-a",
        }),
        parentConversationId: 10,
        meta: {
          "codeg.delegation": {
            status: "running",
            task_id: "run-2",
            child_conversation_id: 77,
            generation: 2,
            started_at: continuedAt,
            runtime_stats: runtimeStats(continuedAt, 2),
          },
        },
      },
    ]

    renderWithIntl(
      <SubAgentOverlay
        delegations={delegations}
        overlayKey="k-sticky-runtime"
      />
    )

    expect(screen.getAllByTestId("sub-agent-row")).toHaveLength(1)
    expect(screen.getByTestId("delegation-operational")).toHaveTextContent(
      "Streaming |"
    )
    expect(screen.getByTestId("delegation-operational")).toHaveTextContent(
      "7 tool uses"
    )
  })

  it("does not group an uncorrelated failed continuation with its old child", () => {
    const groups = groupDelegationSourcesForOverlay([
      {
        ...source("pt-1", { agent_type: "codex", task: "First run" }),
        output: JSON.stringify({
          status: "completed",
          task_id: "run-1",
          child_conversation_id: 77,
        }),
      },
      {
        ...source("pt-2", {
          task_id: "run-1",
          task: "Retry the old child",
        }),
        output: JSON.stringify({
          status: "failed",
          child_conversation_id: 77,
          error_code: "continuation_not_resumable",
          message: "No new run was reserved.",
        }),
      },
    ])

    expect(groups).toHaveLength(2)
    expect(groups[0]).toMatchObject({
      childConversationId: 77,
      runCount: 1,
    })
    expect(groups[1]).toMatchObject({
      key: "source:pt-2",
      childConversationId: null,
      runCount: 1,
    })
  })

  it("regroups cold snapshot-only cards after their DTOs identify a shared child", () => {
    const parentConversationId = 10
    const delegations: DelegationCardSource[] = [
      {
        ...source("pt-1", { agent_type: "codex", task: "First review" }),
        parentConversationId,
        output: JSON.stringify({ task_id: "run-1" }),
      },
      {
        ...source("pt-2", { agent_type: "codex", task: "Second review" }),
        parentConversationId,
        output: JSON.stringify({ task_id: "run-2" }),
      },
    ]

    renderWithIntl(
      <SubAgentOverlay delegations={delegations} overlayKey="k-cold-group" />
    )
    expect(screen.getAllByTestId("sub-agent-row")).toHaveLength(2)

    act(() => {
      delegationRunSnapshotCache.install(
        snapshotCacheKey(parentConversationId, "run-1"),
        snapshotFor("run-1", 1, 77)
      )
      delegationRunSnapshotCache.install(
        snapshotCacheKey(parentConversationId, "run-2"),
        snapshotFor("run-2", 2, 77)
      )
    })

    expect(screen.getByTestId("sub-agent-overlay-group-77")).toHaveTextContent(
      "2 runs"
    )
    expect(screen.getAllByTestId("sub-agent-row")).toHaveLength(1)
  })

  it("opens the shared session tab for the latest run", () => {
    const parentConversationId = 10
    delegationRunSnapshotCache.install(
      snapshotCacheKey(parentConversationId, "run-2"),
      {
        ...snapshotFor("run-2", 2, 77),
        child_turn_anchor: "turn-42",
      }
    )
    renderWithIntl(
      <SubAgentOverlay
        delegations={[
          {
            ...source("pt-2", {
              agent_type: "codex",
              task: "Second review",
            }),
            parentConversationId,
            output: JSON.stringify({ task_id: "run-2" }),
          },
        ]}
        overlayKey="k-anchor"
      />
    )

    fireEvent.click(screen.getByTestId("sub-agent-open"))
    expect(openDelegatedChildSession).toHaveBeenCalledWith(
      expect.objectContaining({
        childConversationId: 77,
        agentType: "codex",
        title: "Second review",
      })
    )
    expect(
      screen.queryByTestId("sub-agent-session-dialog")
    ).not.toBeInTheDocument()
  })

  it("keeps long multi-line task text to a single truncated line", () => {
    // Multi-line tasks (and any long shell-context noise) must stay one line
    // with ellipsis — not expand the overlay card via break-words.
    const longTask =
      "Line one of a very long delegated task description.\n" +
      "Line two keeps going with more context that used to wrap.\n" +
      "Line three would inflate the overlay without truncate."
    renderWithIntl(
      <SubAgentOverlay
        delegations={[source("pt-1", { agent_type: "codex", task: longTask })]}
        overlayKey="k-truncate"
      />
    )
    const secondary = screen.getByTestId("delegation-secondary")
    expect(secondary).toHaveClass("truncate")
    expect(secondary).not.toHaveClass("break-words")
    expect(secondary).toHaveAttribute("title", longTask)
    expect(secondary).toHaveTextContent("Line one of a very long")
  })

  it("collapses to a pill summarizing the count when defaultExpanded is false", () => {
    const delegations = [
      source("pt-1", { agent_type: "codex", task: "Investigate flaky test" }),
      source("pt-2", { agent_type: "claude_code", task: "Write the fix" }),
    ]
    renderWithIntl(
      <SubAgentOverlay
        delegations={delegations}
        overlayKey="k-collapsed"
        defaultExpanded={false}
      />
    )
    expect(screen.getByText("Sub-agents 2")).toBeInTheDocument()
    // Rows are hidden while collapsed.
    expect(screen.queryByText("Investigate flaky test")).not.toBeInTheDocument()
  })

  it("clicking the pill expands the list with icon/name/task per sub-agent", () => {
    const delegations = [
      source("pt-1", { agent_type: "codex", task: "Investigate flaky test" }),
      source("pt-2", { agent_type: "claude_code", task: "Write the fix" }),
    ]
    renderWithIntl(
      <SubAgentOverlay
        delegations={delegations}
        overlayKey="k-2"
        defaultExpanded={false}
      />
    )
    fireEvent.click(screen.getByText("Sub-agents 2").closest("button")!)

    // Header title + both rows (one per delegation).
    expect(screen.getByText("Sub-agents")).toBeInTheDocument()
    expect(screen.getAllByTestId("sub-agent-row")).toHaveLength(2)
    expect(screen.getByText("Investigate flaky test")).toBeInTheDocument()
    expect(screen.getByText("Write the fix")).toBeInTheDocument()
  })

  it("opens the child conversation tab from the separate open control", () => {
    bindings["pt-1"] = bindingOf({
      parentToolUseId: "pt-1",
      childConversationId: 77,
      agentType: "codex",
      status: "running",
      task: "Investigate flaky test",
    })
    const delegations = [
      source("pt-1", { agent_type: "codex", task: "Investigate flaky test" }),
    ]
    renderWithIntl(
      <SubAgentOverlay
        delegations={delegations}
        overlayKey="k-3"
        defaultExpanded
      />
    )
    expect(
      screen.queryByTestId("sub-agent-session-dialog")
    ).not.toBeInTheDocument()

    // Row container is not a button — open is a sibling control.
    const row = screen.getByTestId("sub-agent-row")
    expect(row.tagName.toLowerCase()).toBe("div")
    fireEvent.click(screen.getByTestId("sub-agent-open"))

    expect(openDelegatedChildSession).toHaveBeenCalledWith(
      expect.objectContaining({
        childConversationId: 77,
        agentType: "codex",
      })
    )
    expect(
      screen.queryByTestId("sub-agent-session-dialog")
    ).not.toBeInTheDocument()
  })

  it("expands touched files on the row without opening the session dialog", () => {
    bindings["pt-1"] = bindingOf({
      parentToolUseId: "pt-1",
      childConversationId: 77,
      status: "running",
      runtimeStats: {
        started_at: "2026-07-19T00:00:00.000Z",
        tool_call_count: 4,
        edit_tool_call_count: 1,
        touched_files: [
          {
            path: "src/overlay.ts",
            outside_workspace: false,
            additions: 1,
            deletions: 0,
          },
        ],
        touched_files_truncated: false,
        line_counts_complete: true,
        additions: 1,
        deletions: 0,
      },
    })
    renderWithIntl(
      <SubAgentOverlay
        delegations={[
          source("pt-1", {
            agent_type: "codex",
            task: "Investigate flaky test",
          }),
        ]}
        overlayKey="k-expand"
        defaultExpanded
      />
    )

    expect(
      screen.queryByTestId("sub-agent-session-dialog")
    ).not.toBeInTheDocument()
    fireEvent.click(screen.getByTestId("delegation-files-toggle"))
    expect(screen.getByTestId("delegation-files-panel")).toHaveTextContent(
      "src/overlay.ts"
    )
    // Expand must not open the child conversation tab.
    expect(openDelegatedChildSession).not.toHaveBeenCalled()
    expect(
      screen.queryByTestId("sub-agent-session-dialog")
    ).not.toBeInTheDocument()
  })

  it("renders a graceful fallback row for a delegation with unparseable input", () => {
    const delegations = [
      source("pt-1", { agent_type: "codex", task: "Real task" }),
      { parentToolUseId: "pt-2", input: "not-json" } as DelegationCardSource,
    ]
    renderWithIntl(
      <SubAgentOverlay
        delegations={delegations}
        overlayKey="k-4"
        defaultExpanded
      />
    )
    // The collapsed count never disagrees with the list: both rows render.
    expect(screen.getAllByTestId("sub-agent-row")).toHaveLength(2)
    expect(screen.getByText("Real task")).toBeInTheDocument()
    // The unresolvable one degrades to the "Sub-agent" (unknown agent) label.
    expect(screen.getByText("Sub-agent")).toBeInTheDocument()
  })

  it("renders fallback rows even when every delegation is unresolvable", () => {
    const delegations = [
      { parentToolUseId: "pt-1", input: "not-json" } as DelegationCardSource,
      { parentToolUseId: "pt-2", input: "also-bad" } as DelegationCardSource,
    ]
    renderWithIntl(
      <SubAgentOverlay
        delegations={delegations}
        overlayKey="k-5"
        defaultExpanded
      />
    )
    expect(screen.getAllByTestId("sub-agent-row")).toHaveLength(2)
    expect(screen.getAllByText("Sub-agent")).toHaveLength(2)
  })

  it("shows the broker task id (short, #-prefixed) after each agent name", () => {
    const delegations: DelegationCardSource[] = [
      {
        ...source("pt-1", { agent_type: "codex", task: "Investigate" }),
        // The ack output carries the broker-minted task_id.
        output: JSON.stringify({ task_id: "abc12345def67890" }),
      },
    ]
    renderWithIntl(
      <SubAgentOverlay
        delegations={delegations}
        overlayKey="k-taskid"
        defaultExpanded
      />
    )
    // Truncated to 8 chars in the row, full id in the tooltip.
    const badge = screen.getByText("#abc12345")
    expect(badge).toBeInTheDocument()
    expect(badge).toHaveAttribute("title", "abc12345def67890")
  })

  it("omits the task id badge when no id has been minted yet", () => {
    const delegations = [
      source("pt-1", { agent_type: "codex", task: "Investigate" }),
    ]
    renderWithIntl(
      <SubAgentOverlay
        delegations={delegations}
        overlayKey="k-noid"
        defaultExpanded
      />
    )
    expect(screen.getByTestId("sub-agent-row")).toBeInTheDocument()
    expect(screen.queryByText(/^#/)).not.toBeInTheDocument()
  })

  it("renders no cancel action for native activity", () => {
    const nativeRunningActivity =
      (): import("@/lib/types").DelegationActivityView => ({
        origin: "native",
        authoritative: false,
        platform: "codex",
        operation: "spawn",
        observed_status: "running",
        task_id: "agent-native-1",
        started_at: "2026-07-16T10:00:00Z",
        updated_at: "2026-07-16T10:00:00Z",
      })
    renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[nativeRunningActivity()]}
        overlayKey="k-native"
        defaultExpanded
      />
    )
    expect(screen.getByTestId("sub-agent-origin-native")).toBeInTheDocument()
    const row = screen.getByTestId("sub-agent-row")
    expect(row).toHaveAttribute("data-origin", "native")
    expect(row).toHaveAttribute("data-authoritative", "false")
    // Native row is an informational div, not a button / dialog trigger.
    expect(row.tagName.toLowerCase()).toBe("div")
    expect(row.closest("button")).toBeNull()
    expect(
      screen.queryByRole("button", { name: /cancel/i })
    ).not.toBeInTheDocument()
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument()
  })

  it("renders native role, summary, model, elapsed, and localized operation", () => {
    const native: import("@/lib/types").DelegationActivityView = {
      origin: "native",
      authoritative: false,
      platform: "grok",
      operation: "spawn",
      observed_status: "completed",
      task_id: "agent-native-rich",
      summary: "Audit LOD cold path",
      role: "explore",
      model: "grok-build",
      tool_call_id: "call-rich-1",
      started_at: "2026-07-16T10:00:00Z",
      updated_at: "2026-07-16T10:00:45Z",
      finished_at: "2026-07-16T10:00:45Z",
    }
    renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        activities={[native]}
        overlayKey="k-native-rich"
        defaultExpanded
      />
    )
    const row = screen.getByTestId("sub-agent-row")
    expect(row).toHaveAttribute("data-tool-call-id", "call-rich-1")
    expect(screen.getByText("explore: Audit LOD cold path")).toBeInTheDocument()
    expect(screen.getByText("grok-build")).toBeInTheDocument()
    expect(screen.getByText("Spawn")).toBeInTheDocument()
    expect(screen.getByTestId("sub-agent-native-elapsed")).toHaveTextContent(
      /45s|0m 45s|45/
    )
    expect(
      screen.queryByRole("button", { name: /cancel/i })
    ).not.toBeInTheDocument()
  })

  it("groups Codeg and native activity with origin labels", () => {
    const delegations = [
      source("pt-1", { agent_type: "codex", task: "Codeg child work" }),
    ]
    const native: import("@/lib/types").DelegationActivityView = {
      origin: "native",
      authoritative: false,
      platform: "grok",
      operation: "spawn",
      observed_status: "running",
      started_at: "2026-07-16T10:00:00Z",
      updated_at: "2026-07-16T10:00:00Z",
    }
    renderWithIntl(
      <SubAgentOverlay
        delegations={delegations}
        activities={[native]}
        overlayKey="k-mixed"
        defaultExpanded
      />
    )
    expect(screen.getByTestId("sub-agent-origin-codeg")).toBeInTheDocument()
    expect(screen.getByTestId("sub-agent-origin-native")).toBeInTheDocument()
    expect(screen.getByText("Codeg")).toBeInTheDocument()
    expect(screen.getByText("Native")).toBeInTheDocument()
    expect(
      screen.queryByRole("button", { name: /cancel/i })
    ).not.toBeInTheDocument()
  })

  it("defaults list max-height to 384 and card width to 288", () => {
    renderWithIntl(
      <SubAgentOverlay
        delegations={[
          source("pt-1", {
            agent_type: "codex",
            task: "Investigate flaky test",
          }),
        ]}
        overlayKey="k-size-default"
      />
    )
    const list = screen.getByTestId("sub-agent-overlay-list")
    expect(list).toHaveStyle({ maxHeight: "384px" })
    expect(screen.getByTestId("sub-agent-overlay")).toHaveStyle({
      width: "288px",
    })
  })

  it("hydrates width and maxHeight from localStorage", async () => {
    vi.useFakeTimers()
    localStorage.setItem(
      "workspace:sub-agent-overlay-size",
      JSON.stringify({ width: 360, maxHeight: 420 })
    )
    try {
      renderWithIntl(
        <SubAgentOverlay
          delegations={[
            source("pt-1", {
              agent_type: "codex",
              task: "Investigate flaky test",
            }),
          ]}
          overlayKey="k-size-hydrate"
        />
      )
      act(() => {
        vi.runOnlyPendingTimers()
      })
      expect(screen.getByTestId("sub-agent-overlay")).toHaveStyle({
        width: "360px",
      })
      expect(screen.getByTestId("sub-agent-overlay-list")).toHaveStyle({
        maxHeight: "420px",
      })
    } finally {
      vi.useRealTimers()
    }
  })

  it("exposes resize handles for width, height, and corner", () => {
    renderWithIntl(
      <SubAgentOverlay
        delegations={[
          source("pt-1", {
            agent_type: "codex",
            task: "Investigate flaky test",
          }),
        ]}
        overlayKey="k-size-handles"
      />
    )
    expect(screen.getByTestId("sub-agent-overlay-resize-x")).toHaveAttribute(
      "aria-orientation",
      "vertical"
    )
    expect(screen.getByTestId("sub-agent-overlay-resize-y")).toHaveAttribute(
      "aria-orientation",
      "horizontal"
    )
    expect(
      screen.getByTestId("sub-agent-overlay-resize-xy")
    ).toBeInTheDocument()
  })

  it("persists width after a horizontal resize drag", async () => {
    localStorage.clear()
    renderWithIntl(
      <SubAgentOverlay
        delegations={[
          source("pt-1", {
            agent_type: "codex",
            task: "Investigate flaky test",
          }),
        ]}
        overlayKey="k-size-drag-x"
      />
    )
    const handle = screen.getByTestId("sub-agent-overlay-resize-x")

    // jsdom has no PointerEvent; synthesize pointer* with client coords so the
    // window-level listeners installed by beginResize receive usable deltas.
    const firePointer = (
      target: EventTarget,
      type: string,
      clientX: number,
      clientY: number
    ) => {
      const ev = new Event(type, { bubbles: true, cancelable: true })
      Object.defineProperty(ev, "clientX", { value: clientX })
      Object.defineProperty(ev, "clientY", { value: clientY })
      Object.defineProperty(ev, "pointerId", { value: 1 })
      target.dispatchEvent(ev)
    }

    act(() => {
      firePointer(handle, "pointerdown", 300, 100)
      firePointer(window, "pointermove", 380, 100)
      firePointer(window, "pointerup", 380, 100)
    })

    await vi.waitFor(() => {
      expect(screen.getByTestId("sub-agent-overlay")).toHaveStyle({
        width: "368px",
      })
    })
    expect(
      JSON.parse(localStorage.getItem("workspace:sub-agent-overlay-size")!)
    ).toMatchObject({ width: 368 })
  })
})
