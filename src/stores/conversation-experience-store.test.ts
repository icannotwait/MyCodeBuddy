import { beforeEach, describe, expect, it, vi } from "vitest"
import type { ConversationExperienceSettings } from "@/lib/types"

const h = vi.hoisted(() => ({
  getSettings: vi.fn(),
  setApiConfig: vi.fn(),
  setTranslateAgent: vi.fn(),
  setLimit: vi.fn(),
  subscribe: vi.fn(async () => () => {}),
  onReconnect: vi.fn(() => () => {}),
}))

vi.mock("@/lib/api", () => ({
  getConversationExperienceSettings: h.getSettings,
  setAutoTitleApiConfig: h.setApiConfig,
  setDocumentTranslateAgent: h.setTranslateAgent,
  setReferenceSearchLimit: h.setLimit,
}))

vi.mock("@/lib/platform", () => ({
  subscribe: h.subscribe,
  onTransportReconnect: h.onReconnect,
}))

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

beforeEach(() => {
  resetConversationExperienceStore()
  h.getSettings.mockReset()
  h.setApiConfig.mockReset()
  h.setTranslateAgent.mockReset()
  h.setLimit.mockReset()
  h.subscribe.mockReset()
  h.subscribe.mockResolvedValue(() => {})
  h.onReconnect.mockReset()
  h.onReconnect.mockReturnValue(() => {})
})

describe("useConversationExperienceStore", () => {
  it("drops reordered settings responses and events", () => {
    const store = useConversationExperienceStore.getState()
    store.applySnapshot(
      doc({
        auto_title_api_url: "https://api.example.com/v1",
        auto_title_api_key_set: true,
        auto_title_model: "gpt-4o-mini",
        revision: 4,
      })
    )
    store.applySnapshot(
      doc({
        auto_title_api_url: "",
        revision: 3,
      })
    )
    expect(useConversationExperienceStore.getState().settings?.revision).toBe(4)
    expect(
      useConversationExperienceStore.getState().settings?.auto_title_model
    ).toBe("gpt-4o-mini")
  })

  it("initialize is idempotent for subscription and initial fetch", async () => {
    h.getSettings.mockResolvedValue(doc({ revision: 1 }))
    const store = useConversationExperienceStore.getState()
    store.initialize()
    store.initialize()
    expect(h.subscribe).toHaveBeenCalledTimes(1)
    expect(h.onReconnect).toHaveBeenCalledTimes(1)
    await vi.waitFor(() => {
      expect(h.getSettings).toHaveBeenCalledTimes(1)
    })
  })

  it("setAutoTitleApiConfig applies the returned full document", async () => {
    h.setApiConfig.mockResolvedValue(
      doc({
        auto_title_api_url: "https://api.example.com/v1",
        auto_title_api_key_set: true,
        auto_title_model: "gpt-4o-mini",
        revision: 2,
      })
    )
    await useConversationExperienceStore.getState().setAutoTitleApiConfig({
      api_url: "https://api.example.com/v1",
      api_key_update: { set: "sk-new" },
      model: "gpt-4o-mini",
    })
    expect(h.setApiConfig).toHaveBeenCalledWith({
      api_url: "https://api.example.com/v1",
      api_key_update: { set: "sk-new" },
      model: "gpt-4o-mini",
    })
    expect(useConversationExperienceStore.getState().settings).toEqual(
      doc({
        auto_title_api_url: "https://api.example.com/v1",
        auto_title_api_key_set: true,
        auto_title_model: "gpt-4o-mini",
        revision: 2,
      })
    )
  })

  it("setDocumentTranslateAgent applies the returned full document", async () => {
    h.setTranslateAgent.mockResolvedValue(
      doc({
        document_translate_agent: "codex",
        revision: 2,
      })
    )
    await useConversationExperienceStore
      .getState()
      .setDocumentTranslateAgent("codex")
    expect(h.setTranslateAgent).toHaveBeenCalledWith("codex")
    expect(
      useConversationExperienceStore.getState().settings
        ?.document_translate_agent
    ).toBe("codex")
  })

  it("setReferenceSearchLimit applies the returned full document", async () => {
    h.setLimit.mockResolvedValue(
      doc({
        reference_search_limit: 25,
        revision: 3,
      })
    )
    await useConversationExperienceStore.getState().setReferenceSearchLimit(25)
    expect(h.setLimit).toHaveBeenCalledWith(25)
    expect(useConversationExperienceStore.getState().settings).toEqual(
      doc({
        reference_search_limit: 25,
        revision: 3,
      })
    )
  })
})
