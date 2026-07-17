import { beforeEach, describe, expect, it, vi } from "vitest"

const h = vi.hoisted(() => ({
  getSettings: vi.fn(),
  setAgent: vi.fn(),
  setLimit: vi.fn(),
  subscribe: vi.fn(async () => () => {}),
  onReconnect: vi.fn(() => () => {}),
}))

vi.mock("@/lib/api", () => ({
  getConversationExperienceSettings: h.getSettings,
  setAutoTitleAgent: h.setAgent,
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

beforeEach(() => {
  resetConversationExperienceStore()
  h.getSettings.mockReset()
  h.setAgent.mockReset()
  h.setLimit.mockReset()
  h.subscribe.mockReset()
  h.subscribe.mockResolvedValue(() => {})
  h.onReconnect.mockReset()
  h.onReconnect.mockReturnValue(() => {})
})

describe("useConversationExperienceStore", () => {
  it("drops reordered settings responses and events", () => {
    const store = useConversationExperienceStore.getState()
    store.applySnapshot({
      auto_title_agent: "codex",
      reference_search_limit: 50,
      revision: 4,
    })
    store.applySnapshot({
      auto_title_agent: null,
      reference_search_limit: 50,
      revision: 3,
    })
    expect(useConversationExperienceStore.getState().settings?.revision).toBe(4)
    expect(
      useConversationExperienceStore.getState().settings?.auto_title_agent
    ).toBe("codex")
  })

  it("initialize is idempotent for subscription and initial fetch", async () => {
    h.getSettings.mockResolvedValue({
      auto_title_agent: null,
      reference_search_limit: 50,
      revision: 1,
    })
    const store = useConversationExperienceStore.getState()
    store.initialize()
    store.initialize()
    expect(h.subscribe).toHaveBeenCalledTimes(1)
    expect(h.onReconnect).toHaveBeenCalledTimes(1)
    await vi.waitFor(() => {
      expect(h.getSettings).toHaveBeenCalledTimes(1)
    })
  })

  it("setAutoTitleAgent applies the returned full document", async () => {
    h.setAgent.mockResolvedValue({
      auto_title_agent: "codex",
      reference_search_limit: 50,
      revision: 2,
    })
    await useConversationExperienceStore.getState().setAutoTitleAgent("codex")
    expect(h.setAgent).toHaveBeenCalledWith("codex")
    expect(useConversationExperienceStore.getState().settings).toEqual({
      auto_title_agent: "codex",
      reference_search_limit: 50,
      revision: 2,
    })
  })

  it("setReferenceSearchLimit applies the returned full document", async () => {
    h.setLimit.mockResolvedValue({
      auto_title_agent: null,
      reference_search_limit: 25,
      revision: 3,
    })
    await useConversationExperienceStore
      .getState()
      .setReferenceSearchLimit(25)
    expect(h.setLimit).toHaveBeenCalledWith(25)
    expect(useConversationExperienceStore.getState().settings).toEqual({
      auto_title_agent: null,
      reference_search_limit: 25,
      revision: 3,
    })
  })
})
