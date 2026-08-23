import { type ReactNode } from "react"
import { render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { describe, expect, it, vi } from "vitest"

// The create-task action pulls workbench-route + tab-store contexts that this
// unit test doesn't mount; stub it to a no-op handler.
vi.mock("./use-create-task-from-message", () => ({
  useCreateTaskFromMessage: () => () => {},
}))

import { TurnStats } from "./turn-stats"
import { MessageScrollProvider } from "./message-scroll-context"
import enMessages from "@/i18n/messages/en.json"

function renderStats(ui: ReactNode) {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <MessageScrollProvider value={{ scrollToIndex: vi.fn() }}>
        {ui}
      </MessageScrollProvider>
    </NextIntlClientProvider>
  )
}

const jumpLabel = enMessages.Folder.chat.messageList.jumpToPreviousUserMessage

describe("TurnStats jump-to-previous-user gating", () => {
  it("shows the jump button for a duration-only turn (no token usage)", () => {
    // Cursor never reports per-turn token usage; a turn that still carries a
    // duration is a substantial reply and must keep the jump affordance.
    renderStats(
      <TurnStats
        copyText="hello"
        duration_ms={42_000}
        previousUserIndex={3}
        usage={null}
      />
    )
    expect(screen.getByLabelText(jumpLabel)).toBeInTheDocument()
  })

  it("keeps the jump button hidden when neither usage nor duration exists", () => {
    renderStats(
      <TurnStats
        copyText="hello"
        duration_ms={null}
        previousUserIndex={3}
        usage={null}
      />
    )
    expect(screen.queryByLabelText(jumpLabel)).not.toBeInTheDocument()
  })
})

describe("TurnStats generation speed gating", () => {
  const speedAria = enMessages.Folder.chat.liveTurnStats.outputSpeedAria

  it("hides historical tok/s for agents without a request-usage adapter", () => {
    renderStats(
      <TurnStats
        copyText="hello"
        agentType="cursor"
        generationMs={2000}
        generationTokens={400}
      />
    )
    expect(screen.queryByLabelText(speedAria)).not.toBeInTheDocument()
  })

  it("shows tok/s without fabricating a 100% wall share", () => {
    renderStats(
      <TurnStats
        copyText="hello"
        agentType="claude_code"
        generationMs={2000}
        generationTokens={400}
      />
    )
    expect(screen.getByLabelText(speedAria)).toBeInTheDocument()
    expect(screen.queryByText(/100%/)).not.toBeInTheDocument()
  })
})

describe("TurnStats model and reasoning effort metadata", () => {
  it("shows model and archived reasoning effort in the footer", () => {
    const view = renderStats(
      <TurnStats copyText="reply" model="gpt-5.6-sol" reasoningEffort="high" />
    )

    expect(screen.getByLabelText("Model")).toBeInTheDocument()
    expect(screen.getByLabelText("Reasoning effort")).toBeInTheDocument()

    view.rerender(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <MessageScrollProvider value={{ scrollToIndex: vi.fn() }}>
          <TurnStats copyText="reply" model="gpt-5.6-sol" />
        </MessageScrollProvider>
      </NextIntlClientProvider>
    )
    expect(screen.queryByLabelText("Reasoning effort")).not.toBeInTheDocument()
  })

  it("renders metadata even when a turn has no copyable body", () => {
    renderStats(
      <TurnStats model="gpt-5.6-sol" reasoningEffort="medium" copyText="" />
    )

    expect(screen.getByLabelText("Model")).toBeInTheDocument()
    expect(screen.getByLabelText("Reasoning effort")).toBeInTheDocument()
  })
})
