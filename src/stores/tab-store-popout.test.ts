import { beforeEach, describe, expect, it, vi } from "vitest"

vi.mock("@/lib/api", () => ({
  listOpenedTabs: vi.fn(async () => []),
  saveOpenedTabs: vi.fn(async () => ({
    accepted: true,
    version: 1,
    tabs: [],
  })),
  getFolderConversation: vi.fn(),
}))

vi.mock("@/lib/platform", () => ({
  subscribe: vi.fn(async () => () => {}),
  onTransportReconnect: vi.fn(() => () => {}),
}))

import { resetTabStore, useTabStore } from "@/stores/tab-store"

describe("detachTab", () => {
  beforeEach(() => {
    resetTabStore()
    useTabStore.setState({
      rawTabs: [
        {
          id: "a",
          kind: "conversation",
          folderId: 1,
          conversationId: 1,
          agentType: "claude_code",
          title: "A",
          isPinned: false,
          activationSeq: 1,
        },
        {
          id: "b",
          kind: "conversation",
          folderId: 1,
          conversationId: 2,
          agentType: "claude_code",
          title: "B",
          isPinned: false,
          activationSeq: 2,
        },
      ],
      activeTabId: "a",
      tabsHydrated: true,
    })
  })

  it("refuses last tab", () => {
    useTabStore.setState({
      rawTabs: [
        {
          id: "a",
          kind: "conversation",
          folderId: 1,
          conversationId: 1,
          agentType: "claude_code",
          title: "A",
          isPinned: false,
        },
      ],
      activeTabId: "a",
      tabsHydrated: true,
    })
    expect(useTabStore.getState().detachTab("a")).toEqual({
      ok: false,
      reason: "last_tab",
    })
  })

  it("activates MRU remaining and returns restore token", () => {
    const result = useTabStore.getState().detachTab("a")
    expect(result.ok).toBe(true)
    if (!result.ok) return
    expect(result.nextActiveId).toBe("b")
    expect(useTabStore.getState().rawTabs.map((t) => t.id)).toEqual(["b"])
    expect(result.restoreToken.tab.id).toBe("a")

    useTabStore.getState().restoreDetachedTab(result.restoreToken)
    expect(useTabStore.getState().rawTabs.map((t) => t.id)).toContain("a")
  })
})
