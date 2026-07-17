import {
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"

const h = vi.hoisted(() => ({
  agents: [] as Array<{
    agent_type: string
    name: string
    enabled: boolean
    available: boolean
  }>,
  setAutoTitleAgent: vi.fn(),
  bootstrap: vi.fn(),
  settings: null as null | {
    auto_title_agent: string | null
    reference_search_limit: number
    revision: number
  },
  loading: false,
}))

vi.mock("@/hooks/use-acp-agents", () => ({
  useAcpAgents: () => ({
    agents: h.agents,
    fresh: true,
    refresh: vi.fn(),
  }),
}))

vi.mock("@/stores/conversation-experience-store", () => ({
  useConversationExperienceBootstrap: h.bootstrap,
  useConversationExperienceStore: (
    selector: (s: {
      settings: typeof h.settings
      loading: boolean
      setAutoTitleAgent: typeof h.setAutoTitleAgent
    }) => unknown
  ) =>
    selector({
      settings: h.settings,
      loading: h.loading,
      setAutoTitleAgent: h.setAutoTitleAgent,
    }),
}))

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn() },
}))

import { ConversationExperienceSettingsSection } from "./conversation-experience-settings"
import enMessages from "@/i18n/messages/en.json"

function renderWithIntl() {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <ConversationExperienceSettingsSection />
    </NextIntlClientProvider>
  )
}

async function openListbox() {
  fireEvent.click(screen.getByRole("combobox"))
  return screen.findByRole("listbox")
}

beforeEach(() => {
  h.agents = [
    {
      agent_type: "codex",
      name: "Codex",
      enabled: true,
      available: true,
    },
    {
      agent_type: "claude_code",
      name: "Claude Code",
      enabled: false,
      available: true,
    },
    {
      agent_type: "gemini",
      name: "Gemini",
      enabled: true,
      available: false,
    },
  ]
  h.settings = {
    auto_title_agent: null,
    reference_search_limit: 50,
    revision: 1,
  }
  h.loading = false
  h.setAutoTitleAgent.mockReset()
  h.bootstrap.mockReset()
})

describe("ConversationExperienceSettingsSection", () => {
  it("lists Off plus enabled-and-available base agents only", async () => {
    renderWithIntl()
    expect(h.bootstrap).toHaveBeenCalled()
    const listbox = await openListbox()
    expect(within(listbox).getByText("Off")).toBeInTheDocument()
    expect(within(listbox).getByText("Codex")).toBeInTheDocument()
    expect(within(listbox).queryByText("Claude Code")).not.toBeInTheDocument()
    expect(within(listbox).queryByText("Gemini")).not.toBeInTheDocument()
  })

  it("retains an unavailable saved agent as a disabled labeled row", async () => {
    h.settings = {
      auto_title_agent: "gemini",
      reference_search_limit: 50,
      revision: 2,
    }
    renderWithIntl()
    const listbox = await openListbox()
    const row = within(listbox).getByText("Gemini (Unavailable)")
    expect(row).toBeInTheDocument()
    const option = row.closest("[role='option']")
    expect(option).toHaveAttribute("data-disabled")
  })

  it("saves the selected agent through the store", async () => {
    h.setAutoTitleAgent.mockResolvedValue({
      auto_title_agent: "codex",
      reference_search_limit: 50,
      revision: 3,
    })
    renderWithIntl()
    const listbox = await openListbox()
    fireEvent.click(within(listbox).getByText("Codex"))
    await waitFor(() => {
      expect(h.setAutoTitleAgent).toHaveBeenCalledWith("codex")
    })
  })
})
