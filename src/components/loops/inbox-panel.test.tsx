import { describe, it, expect, vi, beforeEach } from "vitest"
import { render, screen, fireEvent, waitFor } from "@testing-library/react"

import { InboxPanel } from "./inbox-panel"
import type { LoopInboxItemRow } from "@/lib/types"

// Stable `t` so the panel's `refresh` callback identity is steady (per next-intl
// mock guidance); returns the key verbatim, which is enough to address labels.
const {
  stableT,
  listLoopInbox,
  listLoopIterations,
  approveLoopDesign,
  rejectLoopDesign,
  approveLoopMerge,
  rejectLoopMerge,
  retryLoopIssue,
  addLoopIssueBudget,
  cancelLoopIssue,
  forceCompleteLoopTask,
  overrideLoopOscillation,
  focusArtifact,
  gotoIssue,
  openIteration,
} = vi.hoisted(() => ({
  stableT: (key: string) => key,
  listLoopInbox: vi.fn(),
  listLoopIterations: vi.fn(),
  approveLoopDesign: vi.fn(),
  rejectLoopDesign: vi.fn(),
  approveLoopMerge: vi.fn(),
  rejectLoopMerge: vi.fn(),
  retryLoopIssue: vi.fn(),
  addLoopIssueBudget: vi.fn(),
  cancelLoopIssue: vi.fn(),
  forceCompleteLoopTask: vi.fn(),
  overrideLoopOscillation: vi.fn(),
  focusArtifact: vi.fn(),
  gotoIssue: vi.fn(),
  openIteration: vi.fn(),
}))

vi.mock("next-intl", () => ({ useTranslations: () => stableT }))
vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }))
vi.mock("@/components/loops/loop-realtime-context", () => ({
  useLoopRealtime: () => ({ register: () => () => {} }),
}))
vi.mock("@/hooks/use-loop-nav", () => ({
  useLoopNav: () => ({ focusArtifact, gotoIssue }),
}))
vi.mock("@/components/loops/loop-overlays-context", () => ({
  useLoopOverlays: () => ({ openIteration }),
}))
vi.mock("@/lib/loops-api", () => ({
  listLoopInbox,
  listLoopIterations,
  approveLoopDesign,
  rejectLoopDesign,
  approveLoopMerge,
  rejectLoopMerge,
  retryLoopIssue,
  addLoopIssueBudget,
  cancelLoopIssue,
  forceCompleteLoopTask,
  overrideLoopOscillation,
}))

function item(over: Partial<LoopInboxItemRow> = {}): LoopInboxItemRow {
  return {
    id: 1,
    issue_id: 7,
    issue_seq: 3,
    iteration_id: null,
    kind: "approval",
    subject_key: "design:7",
    payload: { gate: "design" },
    status: "pending",
    subject_artifact_id: null,
    subject_title: null,
    created_at: "2026-06-14T00:00:00Z",
    ...over,
  }
}

const blocked = (over: Partial<LoopInboxItemRow> = {}) =>
  item({
    id: 3,
    kind: "blocked",
    subject_key: "no_progress:9",
    payload: { reason: "max_attempts" },
    ...over,
  })

const budget = (over: Partial<LoopInboxItemRow> = {}) =>
  item({
    id: 4,
    kind: "budget_exhausted",
    subject_key: "budget:7",
    payload: { token_used: 1200, token_budget: 1000 },
    ...over,
  })

beforeEach(() => {
  vi.clearAllMocks()
  // The panel's second resource (iteration→session map) must resolve; tests that
  // exercise "open session" override this with specific rows.
  listLoopIterations.mockResolvedValue([])
})

describe("InboxPanel", () => {
  it("shows the empty state when nothing is pending", async () => {
    listLoopInbox.mockResolvedValue([])
    render(<InboxPanel spaceId={1} />)
    expect(await screen.findByText("empty")).toBeInTheDocument()
  })

  it("approves a design gate", async () => {
    listLoopInbox.mockResolvedValue([item()])
    approveLoopDesign.mockResolvedValue(undefined)
    render(<InboxPanel spaceId={1} />)
    fireEvent.click(await screen.findByRole("button", { name: "approve" }))
    await waitFor(() => expect(approveLoopDesign).toHaveBeenCalledWith(7))
  })

  it("approves a merge gate via the Merge action", async () => {
    listLoopInbox.mockResolvedValue([
      item({ id: 2, subject_key: "merge:7", payload: { gate: "merge" } }),
    ])
    approveLoopMerge.mockResolvedValue(undefined)
    render(<InboxPanel spaceId={1} />)
    fireEvent.click(await screen.findByRole("button", { name: "merge" }))
    await waitFor(() => expect(approveLoopMerge).toHaveBeenCalledWith(7))
  })

  it("rejects a design gate with a comment", async () => {
    listLoopInbox.mockResolvedValue([item()])
    rejectLoopDesign.mockResolvedValue(undefined)
    render(<InboxPanel spaceId={1} />)
    fireEvent.click(await screen.findByRole("button", { name: "reject" }))
    const textarea = await screen.findByPlaceholderText("rejectPlaceholder")
    fireEvent.change(textarea, { target: { value: "need more detail" } })
    fireEvent.click(screen.getByRole("button", { name: "submitReject" }))
    await waitFor(() =>
      expect(rejectLoopDesign).toHaveBeenCalledWith(7, "need more detail")
    )
  })

  it("retries a blocked issue", async () => {
    listLoopInbox.mockResolvedValue([blocked()])
    retryLoopIssue.mockResolvedValue(undefined)
    render(<InboxPanel spaceId={1} />)
    fireEvent.click(await screen.findByRole("button", { name: "retry" }))
    await waitFor(() => expect(retryLoopIssue).toHaveBeenCalledWith(7))
  })

  it("stops a blocked issue", async () => {
    listLoopInbox.mockResolvedValue([blocked()])
    cancelLoopIssue.mockResolvedValue(undefined)
    render(<InboxPanel spaceId={1} />)
    fireEvent.click(await screen.findByRole("button", { name: "stop" }))
    await waitFor(() => expect(cancelLoopIssue).toHaveBeenCalledWith(7))
  })

  it("offers force-complete on an ordinary empty-diff block (D15 primary path)", async () => {
    listLoopInbox.mockResolvedValue([
      blocked({
        subject_artifact_id: 9,
        payload: {
          failure_sig: "empty_diff:implement",
          reason: "max_attempts",
        },
      }),
    ])
    forceCompleteLoopTask.mockResolvedValue(undefined)
    render(<InboxPanel spaceId={1} />)
    // Retry stays the primary exit; force-complete is offered alongside it because
    // the cause is an empty diff — reachable WITHOUT waiting for oscillation escalation.
    expect(
      await screen.findByRole("button", { name: "retry" })
    ).toBeInTheDocument()
    fireEvent.click(screen.getByRole("button", { name: "forceComplete" }))
    await waitFor(() => expect(forceCompleteLoopTask).toHaveBeenCalledWith(9))
  })

  it("does not offer force-complete on a non-empty-diff block", async () => {
    listLoopInbox.mockResolvedValue([
      blocked({
        subject_artifact_id: 9,
        payload: { failure_sig: "validation_failed:x", reason: "max_attempts" },
      }),
    ])
    render(<InboxPanel spaceId={1} />)
    expect(
      await screen.findByRole("button", { name: "retry" })
    ).toBeInTheDocument()
    expect(screen.queryByRole("button", { name: "forceComplete" })).toBeNull()
  })

  it("adds budget to a budget-exhausted issue", async () => {
    listLoopInbox.mockResolvedValue([budget()])
    addLoopIssueBudget.mockResolvedValue(undefined)
    render(<InboxPanel spaceId={1} />)
    fireEvent.click(await screen.findByRole("button", { name: "addBudget" }))
    const input = await screen.findByPlaceholderText("addBudgetPlaceholder")
    fireEvent.change(input, { target: { value: "5000" } })
    fireEvent.click(screen.getByRole("button", { name: "submitAddBudget" }))
    await waitFor(() =>
      expect(addLoopIssueBudget).toHaveBeenCalledWith(7, 5000)
    )
  })

  it("opens the conversation for a question card", async () => {
    listLoopInbox.mockResolvedValue([
      item({
        id: 5,
        kind: "question",
        subject_key: "question:5",
        iteration_id: 11,
        payload: { prompt: "Which option?" },
      }),
    ])
    const onOpenQuestion = vi.fn()
    render(<InboxPanel spaceId={1} onOpenQuestion={onOpenQuestion} />)
    expect(await screen.findByText("Which option?")).toBeInTheDocument()
    fireEvent.click(screen.getByRole("button", { name: "openConversation" }))
    expect(onOpenQuestion).toHaveBeenCalledTimes(1)
    expect(onOpenQuestion.mock.calls[0][0].issue_id).toBe(7)
  })

  it("renders both panes and dedupes identical cards", async () => {
    listLoopInbox.mockResolvedValue([
      item(), // design approval (blocking)
      item(), // duplicate identity → deduped away
      item({
        id: 9,
        kind: "question",
        subject_key: "question:5",
        payload: { prompt: "Q?" },
      }),
    ])
    render(<InboxPanel spaceId={1} />)
    expect(await screen.findByText("sectionBlocking")).toBeInTheDocument()
    expect(screen.getByText("sectionQuestions")).toBeInTheDocument()
    // Two identical design cards collapse to one approve action.
    expect(screen.getAllByRole("button", { name: "approve" })).toHaveLength(1)
  })
})
