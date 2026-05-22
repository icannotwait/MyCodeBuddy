import { fireEvent, render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { describe, expect, it, vi } from "vitest"

import { DelegatedSubThread } from "./delegated-sub-thread"
import enMessages from "@/i18n/messages/en.json"
import type { DelegationBinding } from "@/contexts/delegation-context"

vi.mock("@/hooks/use-delegated-sub-session", () => ({
  useDelegatedSubSession: vi.fn(),
}))

const { useDelegatedSubSession } = await import(
  "@/hooks/use-delegated-sub-session"
)
const mockedHook = vi.mocked(useDelegatedSubSession)

function renderWithIntl(ui: React.ReactElement) {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      {ui}
    </NextIntlClientProvider>
  )
}

function bindingOf(overrides: Partial<DelegationBinding>): DelegationBinding {
  return {
    parentConnectionId: "p1",
    parentToolUseId: "pt-1",
    childConnectionId: "c1",
    childConversationId: 99,
    agentType: "codex",
    status: "running",
    ...overrides,
  }
}

describe("DelegatedSubThread", () => {
  it("renders nothing when no binding exists yet", () => {
    mockedHook.mockReturnValue({
      binding: undefined,
      detail: null,
      loading: false,
      error: null,
    })
    const { container } = renderWithIntl(
      <DelegatedSubThread parentToolUseId="pt-1" />
    )
    expect(container.firstChild).toBeNull()
  })

  it("renders agent label + running badge when delegation is in-flight", () => {
    mockedHook.mockReturnValue({
      binding: bindingOf({ status: "running" }),
      detail: null,
      loading: false,
      error: null,
    })
    renderWithIntl(<DelegatedSubThread parentToolUseId="pt-1" />)
    expect(screen.getByText("Codex")).toBeInTheDocument()
    expect(screen.getByText("running")).toBeInTheDocument()
    // collapsed by default — sub-thread body not present
    expect(screen.queryByText(/Loading/)).not.toBeInTheDocument()
  })

  it("shows the error badge with the localized code", () => {
    mockedHook.mockReturnValue({
      binding: bindingOf({ status: "err", errorCode: "timeout" }),
      detail: null,
      loading: false,
      error: null,
    })
    renderWithIntl(<DelegatedSubThread parentToolUseId="pt-1" />)
    expect(screen.getByText("timeout")).toBeInTheDocument()
  })

  it("toggles the body open and shows the last assistant text as summary", () => {
    mockedHook.mockReturnValue({
      binding: bindingOf({ status: "ok" }),
      detail: {
        summary: {
          id: 99,
          folder_id: 1,
          title: null,
          agent_type: "codex",
          status: "completed",
          model: null,
          git_branch: null,
          external_id: null,
          message_count: 1,
          created_at: "2026-05-23T00:00:00Z",
          updated_at: "2026-05-23T00:00:00Z",
        },
        turns: [
          {
            id: "u1",
            role: "user",
            blocks: [{ type: "text", text: "do something" }],
            timestamp: "2026-05-23T00:00:00Z",
          },
          {
            id: "a1",
            role: "assistant",
            blocks: [{ type: "text", text: "delegated answer body" }],
            timestamp: "2026-05-23T00:00:05Z",
          },
        ],
      },
      loading: false,
      error: null,
    })
    renderWithIntl(<DelegatedSubThread parentToolUseId="pt-1" />)
    // Summary line in the header shows the assistant's last text.
    expect(screen.getByText("delegated answer body")).toBeInTheDocument()
    const toggle = screen.getByRole("button")
    fireEvent.click(toggle)
    // Once expanded the sub-thread renders the role label.
    expect(screen.getByText("Assistant")).toBeInTheDocument()
    expect(screen.getByText("User")).toBeInTheDocument()
  })
})
