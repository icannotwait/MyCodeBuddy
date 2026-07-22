import {
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"
import type { ConversationExperienceSettings } from "@/lib/types"

const mocks = vi.hoisted(() => ({
  agents: [] as Array<{
    agent_type: string
    name: string
    enabled: boolean
    available: boolean
  }>,
  getConversationExperienceSettings: vi.fn(),
  setAutoTitleApiConfig: vi.fn(),
  setDocumentTranslateAgent: vi.fn(),
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
  setAutoTitleApiConfig: mocks.setAutoTitleApiConfig,
  setDocumentTranslateAgent: mocks.setDocumentTranslateAgent,
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

function doc(
  overrides: Partial<ConversationExperienceSettings> = {}
): ConversationExperienceSettings {
  return {
    auto_title_api_url: "",
    auto_title_api_key_set: false,
    auto_title_model: "",
    auto_title_config_barrier: false,
    document_translate_agent: null,
    reference_search_limit: 50,
    revision: 1,
    ...overrides,
  }
}

function renderSettings() {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <ConversationExperienceSettingsSection />
    </NextIntlClientProvider>
  )
}

async function openTranslateListbox() {
  fireEvent.click(screen.getByTestId("document-translate-agent"))
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
  mocks.getConversationExperienceSettings.mockResolvedValue(doc())
  mocks.setAutoTitleApiConfig.mockReset()
  mocks.setDocumentTranslateAgent.mockReset()
  mocks.setReferenceSearchLimit.mockReset()
  mocks.subscribe.mockReset()
  mocks.subscribe.mockResolvedValue(() => {})
  mocks.onTransportReconnect.mockReset()
  mocks.onTransportReconnect.mockReturnValue(() => {})
})

describe("ConversationExperienceSettingsSection", () => {
  it("shows enabled status when URL, key, and model are complete", async () => {
    mocks.getConversationExperienceSettings.mockResolvedValue(
      doc({
        auto_title_api_url: "https://api.example.com/v1",
        auto_title_api_key_set: true,
        auto_title_model: "gpt-4o-mini",
        revision: 2,
      })
    )
    renderSettings()
    const status = await screen.findByTestId("auto-title-status")
    expect(status).toHaveAttribute("data-status", "enabled")
    expect(status).toHaveTextContent("Automatic titles: On")
  })

  it("shows barrier status when config barrier is raised", async () => {
    mocks.getConversationExperienceSettings.mockResolvedValue(
      doc({
        auto_title_api_url: "https://api.example.com/v1",
        auto_title_api_key_set: true,
        auto_title_model: "gpt-4o-mini",
        auto_title_config_barrier: true,
        revision: 2,
      })
    )
    renderSettings()
    const status = await screen.findByTestId("auto-title-status")
    expect(status).toHaveAttribute("data-status", "barrier")
    expect(status).toHaveTextContent(
      "Configuration incomplete — re-save or re-enter key"
    )
  })

  it("saves Keep when password is blank (does not clear)", async () => {
    mocks.getConversationExperienceSettings.mockResolvedValue(
      doc({
        auto_title_api_url: "https://api.example.com/v1",
        auto_title_api_key_set: true,
        auto_title_model: "gpt-4o-mini",
        revision: 2,
      })
    )
    mocks.setAutoTitleApiConfig.mockResolvedValue(
      doc({
        auto_title_api_url: "https://api.example.com/v1",
        auto_title_api_key_set: true,
        auto_title_model: "gpt-4o-mini",
        revision: 3,
      })
    )
    renderSettings()
    await waitFor(() => {
      expect(useConversationExperienceStore.getState().settings).not.toBeNull()
    })
    fireEvent.change(screen.getByLabelText("API Base URL"), {
      target: { value: "https://api.example.com/v1" },
    })
    // Leave password blank.
    fireEvent.click(screen.getByTestId("auto-title-save"))
    await waitFor(() => {
      expect(mocks.setAutoTitleApiConfig).toHaveBeenCalledWith({
        api_url: "https://api.example.com/v1",
        model: "gpt-4o-mini",
      })
    })
    const call = mocks.setAutoTitleApiConfig.mock.calls[0]?.[0] as Record<
      string,
      unknown
    >
    expect(call).not.toHaveProperty("api_key_update")
  })

  it("saves Clear when Clear key is used before Save", async () => {
    mocks.getConversationExperienceSettings.mockResolvedValue(
      doc({
        auto_title_api_url: "https://api.example.com/v1",
        auto_title_api_key_set: true,
        auto_title_model: "gpt-4o-mini",
        revision: 2,
      })
    )
    mocks.setAutoTitleApiConfig.mockResolvedValue(
      doc({
        auto_title_api_url: "https://api.example.com/v1",
        auto_title_api_key_set: false,
        auto_title_model: "gpt-4o-mini",
        revision: 3,
      })
    )
    renderSettings()
    await screen.findByTestId("auto-title-clear-key")
    fireEvent.click(screen.getByTestId("auto-title-clear-key"))
    fireEvent.click(screen.getByTestId("auto-title-save"))
    await waitFor(() => {
      expect(mocks.setAutoTitleApiConfig).toHaveBeenCalledWith({
        api_url: "https://api.example.com/v1",
        api_key_update: { clear: true },
        model: "gpt-4o-mini",
      })
    })
  })

  it("preserves pending Clear key across an unrelated settings revision bump", async () => {
    mocks.getConversationExperienceSettings.mockResolvedValue(
      doc({
        auto_title_api_url: "https://api.example.com/v1",
        auto_title_api_key_set: true,
        auto_title_model: "gpt-4o-mini",
        revision: 2,
      })
    )
    // Document-translate save returns a full snapshot with a newer revision
    // but the same title fields — must not wipe local keyCleared.
    mocks.setDocumentTranslateAgent.mockResolvedValue(
      doc({
        auto_title_api_url: "https://api.example.com/v1",
        auto_title_api_key_set: true,
        auto_title_model: "gpt-4o-mini",
        document_translate_agent: "codex",
        revision: 3,
      })
    )
    mocks.setAutoTitleApiConfig.mockResolvedValue(
      doc({
        auto_title_api_url: "https://api.example.com/v1",
        auto_title_api_key_set: false,
        auto_title_model: "gpt-4o-mini",
        document_translate_agent: "codex",
        revision: 4,
      })
    )
    renderSettings()
    await screen.findByTestId("auto-title-clear-key")
    fireEvent.click(screen.getByTestId("auto-title-clear-key"))

    // Clear is pending: button gone, cleared placeholder shown.
    expect(screen.queryByTestId("auto-title-clear-key")).not.toBeInTheDocument()
    expect(screen.getByLabelText("API Key")).toHaveAttribute(
      "placeholder",
      expect.stringMatching(/cleared|re-enter/i)
    )

    // Unrelated settings revision bump via translate agent save.
    const listbox = await openTranslateListbox()
    fireEvent.click(within(listbox).getByText("Codex"))
    await waitFor(() => {
      expect(mocks.setDocumentTranslateAgent).toHaveBeenCalledWith("codex")
    })
    await waitFor(() => {
      expect(useConversationExperienceStore.getState().settings?.revision).toBe(
        3
      )
    })

    // Clear intent must survive the snapshot.
    expect(screen.queryByTestId("auto-title-clear-key")).not.toBeInTheDocument()
    expect(screen.getByLabelText("API Key")).toHaveAttribute(
      "placeholder",
      expect.stringMatching(/cleared|re-enter/i)
    )

    fireEvent.click(screen.getByTestId("auto-title-save"))
    await waitFor(() => {
      expect(mocks.setAutoTitleApiConfig).toHaveBeenCalledWith({
        api_url: "https://api.example.com/v1",
        api_key_update: { clear: true },
        model: "gpt-4o-mini",
      })
    })
  })

  it("saves Set when a new key is typed", async () => {
    mocks.getConversationExperienceSettings.mockResolvedValue(doc())
    mocks.setAutoTitleApiConfig.mockResolvedValue(
      doc({
        auto_title_api_url: "https://api.example.com/v1",
        auto_title_api_key_set: true,
        auto_title_model: "gpt-4o-mini",
        revision: 2,
      })
    )
    renderSettings()
    await waitFor(() => {
      expect(useConversationExperienceStore.getState().settings).not.toBeNull()
    })
    fireEvent.change(screen.getByLabelText("API Base URL"), {
      target: { value: "https://api.example.com/v1" },
    })
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-secret" },
    })
    fireEvent.change(screen.getByLabelText("Model"), {
      target: { value: "gpt-4o-mini" },
    })
    fireEvent.click(screen.getByTestId("auto-title-save"))
    await waitFor(() => {
      expect(mocks.setAutoTitleApiConfig).toHaveBeenCalledWith({
        api_url: "https://api.example.com/v1",
        api_key_update: { set: "sk-secret" },
        model: "gpt-4o-mini",
      })
    })
  })

  it("renders separate title HTTP and translate ACP disclosures", async () => {
    renderSettings()
    await waitFor(() => {
      expect(useConversationExperienceStore.getState().settings).not.toBeNull()
    })
    expect(screen.getByTestId("title-http-disclosure")).toHaveTextContent(
      /configured endpoint for title generation/i
    )
    expect(
      screen.getByTestId("translate-provider-disclosure")
    ).toHaveTextContent(/for translation/i)
    expect(screen.getByTestId("title-http-disclosure").textContent).not.toEqual(
      screen.getByTestId("translate-provider-disclosure").textContent
    )
  })

  it("lists Off plus enabled-and-available base agents for translate", async () => {
    renderSettings()
    await waitFor(() => {
      expect(useConversationExperienceStore.getState().settings).not.toBeNull()
    })
    const listbox = await openTranslateListbox()
    expect(within(listbox).getByText("Off")).toBeInTheDocument()
    expect(within(listbox).getByText("Codex")).toBeInTheDocument()
    expect(within(listbox).queryByText("Claude Code")).not.toBeInTheDocument()
    expect(within(listbox).queryByText("Gemini")).not.toBeInTheDocument()
  })

  it("retains an unavailable saved translate agent as a disabled labeled row", async () => {
    mocks.getConversationExperienceSettings.mockResolvedValue(
      doc({
        document_translate_agent: "gemini",
        revision: 2,
      })
    )
    renderSettings()
    await waitFor(() => {
      expect(
        useConversationExperienceStore.getState().settings
          ?.document_translate_agent
      ).toBe("gemini")
    })
    const listbox = await openTranslateListbox()
    const row = within(listbox).getByText("Gemini (Unavailable)")
    expect(row).toBeInTheDocument()
    const option = row.closest("[role='option']")
    expect(option).toHaveAttribute("data-disabled")
  })

  it("saves the selected translate agent via setDocumentTranslateAgent", async () => {
    mocks.setDocumentTranslateAgent.mockResolvedValue(
      doc({
        document_translate_agent: "codex",
        revision: 3,
      })
    )
    renderSettings()
    await waitFor(() => {
      expect(useConversationExperienceStore.getState().settings).not.toBeNull()
    })
    const listbox = await openTranslateListbox()
    fireEvent.click(within(listbox).getByText("Codex"))
    await waitFor(() => {
      expect(mocks.setDocumentTranslateAgent).toHaveBeenCalledWith("codex")
    })
  })

  it("saves a clamped reference limit and adopts the returned revision", async () => {
    mocks.setReferenceSearchLimit.mockResolvedValue(
      doc({
        reference_search_limit: 500,
        revision: 9,
      })
    )
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
