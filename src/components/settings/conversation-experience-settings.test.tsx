import {
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"

const mocks = vi.hoisted(() => ({
  agents: [] as Array<{
    agent_type: string
    name: string
    enabled: boolean
    available: boolean
  }>,
  getConversationExperienceSettings: vi.fn(),
  setAutoTitleAgent: vi.fn(),
  setReferenceSearchLimit: vi.fn(),
  subscribe: vi.fn(async () => () => {}),
  onTransportReconnect: vi.fn(() => () => {}),
}))

vi.mock("@/hooks/use-acp-agents", () => ({
  useAcpAgents: () => ({
    agents: mocks.agents,
    fresh: true,
    refresh: vi.fn(),
  }),
}))

vi.mock("@/lib/api", () => ({
  getConversationExperienceSettings: mocks.getConversationExperienceSettings,
  setAutoTitleAgent: mocks.setAutoTitleAgent,
  setReferenceSearchLimit: mocks.setReferenceSearchLimit,
}))

vi.mock("@/lib/platform", () => ({
  subscribe: mocks.subscribe,
  onTransportReconnect: mocks.onTransportReconnect,
}))

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn() },
}))

import { ConversationExperienceSettingsSection } from "./conversation-experience-settings"
import enMessages from "@/i18n/messages/en.json"
import {
  resetConversationExperienceStore,
  useConversationExperienceStore,
} from "@/stores/conversation-experience-store"

function renderSettings() {
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
  resetConversationExperienceStore()
  mocks.agents = [
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
  mocks.getConversationExperienceSettings.mockReset()
  mocks.getConversationExperienceSettings.mockResolvedValue({
    auto_title_agent: null,
    reference_search_limit: 50,
    revision: 1,
  })
  mocks.setAutoTitleAgent.mockReset()
  mocks.setReferenceSearchLimit.mockReset()
  mocks.subscribe.mockReset()
  mocks.subscribe.mockResolvedValue(() => {})
  mocks.onTransportReconnect.mockReset()
  mocks.onTransportReconnect.mockReturnValue(() => {})
})

describe("ConversationExperienceSettingsSection", () => {
  it("lists Off plus enabled-and-available base agents only", async () => {
    renderSettings()
    await waitFor(() => {
      expect(useConversationExperienceStore.getState().settings).not.toBeNull()
    })
    const listbox = await openListbox()
    expect(within(listbox).getByText("Off")).toBeInTheDocument()
    expect(within(listbox).getByText("Codex")).toBeInTheDocument()
    expect(within(listbox).queryByText("Claude Code")).not.toBeInTheDocument()
    expect(within(listbox).queryByText("Gemini")).not.toBeInTheDocument()
  })

  it("retains an unavailable saved agent as a disabled labeled row", async () => {
    mocks.getConversationExperienceSettings.mockResolvedValue({
      auto_title_agent: "gemini",
      reference_search_limit: 50,
      revision: 2,
    })
    renderSettings()
    await waitFor(() => {
      expect(
        useConversationExperienceStore.getState().settings?.auto_title_agent
      ).toBe("gemini")
    })
    const listbox = await openListbox()
    const row = within(listbox).getByText("Gemini (Unavailable)")
    expect(row).toBeInTheDocument()
    const option = row.closest("[role='option']")
    expect(option).toHaveAttribute("data-disabled")
  })

  it("saves the selected agent through the store", async () => {
    mocks.setAutoTitleAgent.mockResolvedValue({
      auto_title_agent: "codex",
      reference_search_limit: 50,
      revision: 3,
    })
    renderSettings()
    await waitFor(() => {
      expect(useConversationExperienceStore.getState().settings).not.toBeNull()
    })
    const listbox = await openListbox()
    fireEvent.click(within(listbox).getByText("Codex"))
    await waitFor(() => {
      expect(mocks.setAutoTitleAgent).toHaveBeenCalledWith("codex")
    })
  })

  it("saves a clamped reference limit and adopts the returned revision", async () => {
    mocks.setReferenceSearchLimit.mockResolvedValue({
      auto_title_agent: "codex",
      reference_search_limit: 500,
      revision: 9,
    })
    renderSettings()
    fireEvent.change(await screen.findByLabelText("Reference result limit"), {
      target: { value: "999" },
    })
    fireEvent.click(
      screen.getByRole("button", { name: "Save reference limit" })
    )
    await waitFor(() =>
      expect(mocks.setReferenceSearchLimit).toHaveBeenCalledWith(500)
    )
    expect(useConversationExperienceStore.getState().settings?.revision).toBe(9)
  })
})
