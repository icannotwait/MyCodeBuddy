import { fireEvent, render, screen, waitFor } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

import { IterationDialog } from "./iteration-dialog"
import type { ConnectionState } from "@/contexts/acp-connections-context"
import type { PendingQuestionState } from "@/lib/types"

// next-intl: return a STABLE `t` (per project mock guidance) so effects that
// depend on the translator identity don't re-run every render.
const stableT = (key: string) => key
vi.mock("next-intl", () => ({ useTranslations: () => stableT }))

// The shared child-session hooks are exercised by the sub-agent dialog tests;
// here we stub them so the iteration dialog's own wiring is isolated. The
// connection state is controllable via `mockConn`.
let mockConn: Partial<ConnectionState> | undefined
vi.mock("@/components/message/child-session-hooks", () => ({
  useChildConnectionState: () => mockConn,
  useChildLiveBridge: () => {},
}))

const connectAsViewer = vi.fn()
const disconnect = vi.fn()
const answerQuestion = vi.fn()
const respondPermission = vi.fn()
vi.mock("@/contexts/acp-connections-context", () => ({
  useAcpActions: () => ({
    connectAsViewer,
    disconnect,
    answerQuestion,
    respondPermission,
  }),
}))

const refetchDetail = vi.fn()
const setLiveOwnsActiveTurn = vi.fn()
vi.mock("@/contexts/conversation-runtime-context", () => ({
  useConversationRuntime: () => ({ refetchDetail, setLiveOwnsActiveTurn }),
}))

vi.mock("@/hooks/use-conversation-detail", () => ({
  useConversationDetail: () => ({
    detail: null,
    loading: false,
    error: null,
    acpLoadError: null,
  }),
}))

vi.mock("@/components/message/message-list-view", () => ({
  MessageListView: (props: Record<string, unknown>) => (
    <div
      data-testid="message-list-view"
      data-conversation-id={String(props.conversationId)}
    />
  ),
}))

vi.mock("@/components/chat/permission-dialog", () => ({
  PermissionDialog: ({
    permission,
    onRespond,
  }: {
    permission: { request_id: string } | null
    onRespond: (requestId: string, optionId: string) => void
  }) =>
    permission ? (
      <button
        data-testid="permission"
        onClick={() => onRespond(permission.request_id, "approve")}
      >
        permission
      </button>
    ) : null,
}))

vi.mock("@/components/chat/ask-question-card", () => ({
  AskQuestionCard: ({
    question,
    onAnswer,
  }: {
    question: PendingQuestionState
    onAnswer: (
      questionId: string,
      answer: { answers: unknown[]; declined: boolean }
    ) => void
  }) => (
    <button
      data-testid="ask-question"
      onClick={() =>
        onAnswer(question.question_id, { answers: [], declined: false })
      }
    >
      ask {question.question_id}
    </button>
  ),
}))

function askState(): PendingQuestionState {
  return {
    question_id: "q1",
    created_at: "2026-06-14T00:00:00Z",
    questions: [
      {
        id: "q1-0",
        question: "Which approach?",
        header: "Approach",
        multi_select: false,
        options: [{ label: "A", description: "" }],
      },
    ],
  }
}

beforeEach(() => {
  vi.clearAllMocks()
  mockConn = { status: "prompting" }
})

describe("IterationDialog", () => {
  it("attaches as a viewer on open with the iteration's connection", async () => {
    render(
      <IterationDialog
        open
        onOpenChange={() => {}}
        conversationId={42}
        connectionId="iter-conn"
        agentType="claude_code"
      />
    )
    await waitFor(() =>
      expect(connectAsViewer).toHaveBeenCalledWith(
        "iter-conn",
        "iter-conn",
        "claude_code",
        null
      )
    )
    expect(screen.getByTestId("message-list-view")).toHaveAttribute(
      "data-conversation-id",
      "42"
    )
  })

  it("answers the live question through the iteration connection", async () => {
    mockConn = { status: "prompting", pendingAskQuestion: askState() }
    render(
      <IterationDialog
        open
        onOpenChange={() => {}}
        conversationId={42}
        connectionId="iter-conn"
        agentType="claude_code"
      />
    )
    fireEvent.click(await screen.findByTestId("ask-question"))
    expect(answerQuestion).toHaveBeenCalledWith("iter-conn", "q1", {
      answers: [],
      declined: false,
    })
  })

  it("detaches (not disconnect-the-agent) when closed", async () => {
    const { rerender } = render(
      <IterationDialog
        open
        onOpenChange={() => {}}
        conversationId={42}
        connectionId="iter-conn"
        agentType="claude_code"
      />
    )
    await waitFor(() => expect(connectAsViewer).toHaveBeenCalled())
    // Closing unmounts the body, whose cleanup detaches the viewer.
    rerender(
      <IterationDialog
        open={false}
        onOpenChange={() => {}}
        conversationId={42}
        connectionId="iter-conn"
        agentType="claude_code"
      />
    )
    await waitFor(() => expect(disconnect).toHaveBeenCalledWith("iter-conn"))
  })

  it("does not render the question band when nothing is pending", () => {
    mockConn = { status: "connected" }
    render(
      <IterationDialog
        open
        onOpenChange={() => {}}
        conversationId={42}
        connectionId="iter-conn"
        agentType="claude_code"
      />
    )
    expect(screen.queryByTestId("ask-question")).not.toBeInTheDocument()
    expect(screen.queryByTestId("permission")).not.toBeInTheDocument()
  })
})
