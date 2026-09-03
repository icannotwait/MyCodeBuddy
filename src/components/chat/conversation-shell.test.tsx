import { fireEvent, render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { describe, expect, it, vi } from "vitest"

import enMessages from "@/i18n/messages/en.json"
import type { PendingPermission } from "@/contexts/acp-connections-context"
import type { PromptCapabilitiesInfo } from "@/lib/types"
import type { PendingQuestionState } from "@/lib/types"
import { ConversationShell } from "./conversation-shell"

const CAPS: PromptCapabilitiesInfo = {
  image: true,
  audio: false,
  embedded_context: true,
}

const chatInputProps = vi.hoisted(() => ({
  last: null as null | Record<string, unknown>,
}))
const questionDialogProps = vi.hoisted(() => ({
  last: null as null | Record<string, unknown>,
}))
const askQuestionCardProps = vi.hoisted(() => ({
  last: null as null | Record<string, unknown>,
}))

vi.mock("@/components/chat/chat-input", () => ({
  ChatInput: (props: Record<string, unknown>) => {
    chatInputProps.last = props
    return <div data-testid="mock-chat-input" />
  },
}))

vi.mock("@/components/chat/question-dialog", () => ({
  QuestionDialog: (props: Record<string, unknown>) => {
    questionDialogProps.last = props
    return <div data-testid="mock-question-dialog" />
  },
}))

vi.mock("@/components/chat/ask-question-card", () => ({
  AskQuestionCard: (props: Record<string, unknown>) => {
    askQuestionCardProps.last = props
    return <div data-testid="mock-ask-question-card" />
  },
}))

// Real PermissionDialog — assert approve/reject still works when locked.

function renderShell(
  props: Pick<
    React.ComponentProps<typeof ConversationShell>,
    "interactionLocked"
  > = {}
) {
  const onRespondPermission = vi.fn()
  const onAnswerQuestion = vi.fn()
  const onAnswerAskQuestion = vi.fn()
  const onSend = vi.fn()
  const onCancel = vi.fn()
  const onFocus = vi.fn()

  const pendingPermission: PendingPermission = {
    request_id: "req-1",
    tool_call: {
      toolCallId: "tc-1",
      title: "Edit file",
      kind: "edit",
      status: "pending",
      content: [],
      locations: [],
      rawInput: {},
    },
    options: [
      { option_id: "allow-once", name: "Allow once", kind: "allow_once" },
      { option_id: "reject-once", name: "Reject", kind: "reject_once" },
    ],
  }

  const pendingAskQuestion: PendingQuestionState = {
    question_id: "aq-1",
    created_at: "2026-01-01T00:00:00Z",
    questions: [
      {
        id: "q1",
        question: "Pick one",
        header: "Choice",
        multi_select: false,
        options: [{ label: "A", description: "" }],
      },
    ],
  }

  render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <ConversationShell
        status="connected"
        promptCapabilities={CAPS}
        error={null}
        claudeApiRetry={null}
        pendingPermission={pendingPermission}
        pendingQuestion={{
          tool_call_id: "pq-1",
          question: "Free text?",
        }}
        pendingAskQuestion={pendingAskQuestion}
        pendingPlanApproval={null}
        onFocus={onFocus}
        onSend={onSend}
        onCancel={onCancel}
        onRespondPermission={onRespondPermission}
        onAnswerQuestion={onAnswerQuestion}
        onAnswerAskQuestion={onAnswerAskQuestion}
        onAnswerPlanApproval={vi.fn()}
        interactionLocked={true}
        {...props}
      >
        <div>messages</div>
      </ConversationShell>
    </NextIntlClientProvider>
  )

  return {
    onRespondPermission,
    onAnswerQuestion,
    onAnswerAskQuestion,
    onSend,
    onCancel,
    onFocus,
  }
}

describe("ConversationShell interactionLocked capability", () => {
  it("propagates interactionLocked to ChatInput and both question surfaces", () => {
    renderShell({ interactionLocked: true })

    expect(chatInputProps.last?.interactionLocked).toBe(true)
    expect(questionDialogProps.last?.interactionLocked).toBe(true)
    expect(askQuestionCardProps.last?.interactionLocked).toBe(true)
  })

  it("does not pass interactionLocked into PermissionDialog and still responds", () => {
    const { onRespondPermission } = renderShell({ interactionLocked: true })

    // Real PermissionDialog renders option buttons by name.
    fireEvent.click(screen.getByRole("button", { name: "Allow once" }))
    expect(onRespondPermission).toHaveBeenCalledWith("req-1", "allow-once")

    fireEvent.click(screen.getByRole("button", { name: "Reject" }))
    expect(onRespondPermission).toHaveBeenCalledWith("req-1", "reject-once")
  })

  it("defaults interactionLocked to false when omitted", () => {
    renderShell({ interactionLocked: undefined })
    // Explicit undefined still reaches mocked ChatInput from default param —
    // when the prop is omitted entirely, shell should pass false / undefined
    // that ChatInput treats as unlocked. Re-render without the key.
    render(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <ConversationShell
          status="connected"
          promptCapabilities={CAPS}
          error={null}
          claudeApiRetry={null}
          pendingPermission={null}
          pendingQuestion={null}
          pendingAskQuestion={null}
          pendingPlanApproval={null}
          onFocus={() => {}}
          onSend={() => {}}
          onCancel={() => {}}
          onRespondPermission={() => {}}
          onAnswerQuestion={() => {}}
          onAnswerAskQuestion={() => {}}
          onAnswerPlanApproval={() => {}}
        >
          <div />
        </ConversationShell>
      </NextIntlClientProvider>
    )
    // false default on ChatInput prop when shell omits / passes default false
    expect(
      chatInputProps.last?.interactionLocked === false ||
        chatInputProps.last?.interactionLocked === undefined
    ).toBe(true)
  })
})
