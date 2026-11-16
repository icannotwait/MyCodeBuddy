import { readFileSync } from "node:fs"
import { resolve } from "node:path"
import type { ComponentProps } from "react"
import { fireEvent, render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { describe, expect, it, vi } from "vitest"

import enMessages from "@/i18n/messages/en.json"
import type { PendingPermission } from "@/contexts/acp-connections-context"
import type { PromptCapabilitiesInfo } from "@/lib/types"
import type { PendingQuestionState } from "@/lib/types"

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
    return <div data-testid="composer" />
  },
}))
vi.mock("@/components/chat/chat-width-handles", () => ({
  ChatWidthHandles: () => <div data-testid="width-handles" />,
}))
vi.mock("@/components/chat/composer-status-strips", () => ({
  ComposerStatusStrips: () => null,
}))
vi.mock("@/components/chat/remote-control-strip", () => ({
  RemoteControlStrip: (props: {
    remoteControl: { session_url: string | null }
    onTurnOff?: () => void
    canTurnOff: boolean
  }) => (
    <div
      data-testid="remote-control-strip"
      data-url={String(props.remoteControl.session_url)}
      data-can-turn-off={String(props.canTurnOff)}
      data-has-turn-off={String(props.onTurnOff !== undefined)}
    />
  ),
}))
vi.mock("@/components/chat/permission-dialog", async () => {
  const { useTranscriptRoot } =
    await import("@/components/ai-elements/markdown-local-image")
  return {
    PermissionDialog: function PermissionDialog(props: {
      permission: {
        request_id: string
        options: { option_id: string; name: string }[]
      } | null
      onRespond: (requestId: string, optionId: string) => void
    }) {
      const root = useTranscriptRoot()
      return (
        <div data-testid="permission" data-root={String(root)}>
          {props.permission?.options.map((option) => (
            <button
              key={option.option_id}
              type="button"
              onClick={() =>
                props.onRespond(props.permission!.request_id, option.option_id)
              }
            >
              {option.name}
            </button>
          ))}
        </div>
      )
    },
  }
})
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
vi.mock("@/components/chat/plan-approval-card", async () => {
  const { useTranscriptRoot } =
    await import("@/components/ai-elements/markdown-local-image")
  return {
    PlanApprovalCard: function PlanApprovalCard() {
      const root = useTranscriptRoot()
      return <div data-testid="plan-approval" data-root={String(root)} />
    },
  }
})

import { ConversationShell } from "./conversation-shell"

const CAPS: PromptCapabilitiesInfo = {
  image: true,
  audio: false,
  embedded_context: true,
}

type ShellProps = ComponentProps<typeof ConversationShell>

function renderLockedShell(props: Pick<ShellProps, "interactionLocked"> = {}) {
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

function renderShell(extra: Partial<ShellProps> = {}) {
  const noop = () => {}
  const props = {
    status: null,
    promptCapabilities: {},
    claudeApiRetry: null,
    pendingPermission: null,
    pendingQuestion: null,
    pendingAskQuestion: null,
    pendingPlanApproval: null,
    onFocus: noop,
    onSend: noop,
    onCancel: noop,
    onRespondPermission: noop,
    onAnswerQuestion: noop,
    onAnswerAskQuestion: noop,
    onAnswerPlanApproval: noop,
    ...extra,
  } as unknown as ShellProps
  return render(
    <ConversationShell {...props}>
      <div data-testid="transcript" />
    </ConversationShell>
  )
}

const shellRoot = () =>
  screen.getByTestId("transcript").parentElement!.parentElement!

describe("ConversationShell interactionLocked capability", () => {
  it("propagates interactionLocked to ChatInput and both question surfaces", () => {
    renderLockedShell({ interactionLocked: true })

    expect(chatInputProps.last?.interactionLocked).toBe(true)
    expect(questionDialogProps.last?.interactionLocked).toBe(true)
    expect(askQuestionCardProps.last?.interactionLocked).toBe(true)
  })

  it("does not pass interactionLocked into PermissionDialog and still responds", () => {
    const { onRespondPermission } = renderLockedShell({
      interactionLocked: true,
    })

    fireEvent.click(screen.getByRole("button", { name: "Allow once" }))
    expect(onRespondPermission).toHaveBeenCalledWith("req-1", "allow-once")

    fireEvent.click(screen.getByRole("button", { name: "Reject" }))
    expect(onRespondPermission).toHaveBeenCalledWith("req-1", "reject-once")
  })

  it("defaults interactionLocked to false when omitted", () => {
    render(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <ConversationShell
          status="connected"
          promptCapabilities={CAPS}
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
    expect(
      chatInputProps.last?.interactionLocked === false ||
        chatInputProps.last?.interactionLocked === undefined
    ).toBe(true)
  })
})

describe("ConversationShell chat-width handles", () => {
  it("has neither handles nor a gutter by default (canvas cards)", () => {
    renderShell()
    expect(screen.queryByTestId("width-handles")).toBeNull()
    expect(shellRoot().style.getPropertyValue("--chat-gutter")).toBe("")
  })

  it("adds the handles and the window-edge gutter when resizable", () => {
    renderShell({ resizableWidth: true })
    expect(screen.getByTestId("width-handles")).toBeInTheDocument()
    const host = screen.getByTestId("transcript").parentElement!
    expect(host).toContainElement(screen.getByTestId("width-handles"))
    expect(host).toHaveClass("relative")
    expect(shellRoot().style.getPropertyValue("--chat-gutter")).toBe("12px")
  })
})

describe("who turns the handles on", () => {
  const read = (path: string) =>
    readFileSync(resolve(process.cwd(), path), "utf8")

  it("is the conversation tab, not the zoomable canvas card", () => {
    expect(
      read("src/components/conversations/conversation-session-surface.tsx")
    ).toMatch(/<ConversationShell\s+resizableWidth\s/)
    expect(
      read("src/components/canvas/canvas-conversation-surface.tsx")
    ).not.toContain("resizableWidth")
  })
})

describe("ConversationShell Remote Control strip", () => {
  it("shows Claude Code's Remote Control while it is on", () => {
    renderShell()
    expect(screen.queryByTestId("remote-control-strip")).toBeNull()

    renderShell({
      status: "connected",
      remoteControl: { session_url: "https://claude.ai/code/session_01" },
      onRemoteControlOff: () => {},
    })
    const strip = screen.getByTestId("remote-control-strip")
    expect(strip).toHaveAttribute(
      "data-url",
      "https://claude.ai/code/session_01"
    )
    expect(strip).toHaveAttribute("data-has-turn-off", "true")
    expect(strip).toHaveAttribute("data-can-turn-off", "true")
  })

  it("lets it be turned off only between turns", () => {
    renderShell({
      status: "prompting",
      remoteControl: { session_url: null },
      onRemoteControlOff: () => {},
    })
    expect(screen.getByTestId("remote-control-strip")).toHaveAttribute(
      "data-can-turn-off",
      "false"
    )
  })
})

describe("ConversationShell cards below the transcript", () => {
  it("resolve local images against the conversation's folder", () => {
    renderShell({
      defaultPath: "/repo",
      pendingPlanApproval: { approval_id: "a1" } as never,
    })
    expect(screen.getByTestId("permission")).toHaveAttribute(
      "data-root",
      "/repo"
    )
    expect(screen.getByTestId("plan-approval")).toHaveAttribute(
      "data-root",
      "/repo"
    )
  })
})
