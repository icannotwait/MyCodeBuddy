import { beforeEach, describe, expect, it, vi } from "vitest"

const focusDetachedConversation = vi.fn(async (_id: number) => false)

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
  isLocalDesktop: vi.fn(() => true),
}))

vi.mock("@/lib/conversation-popout", () => ({
  focusDetachedConversation: (id: number) => focusDetachedConversation(id),
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

describe("openTab focus-before-open", () => {
  beforeEach(() => {
    resetTabStore()
    focusDetachedConversation.mockReset()
    focusDetachedConversation.mockResolvedValue(false)
    useTabStore.setState({
      rawTabs: [],
      activeTabId: null,
      tabsHydrated: true,
    })
  })

  it("awaits focus and returns false without adding a main tab when detached focuses", async () => {
    focusDetachedConversation.mockResolvedValue(true)

    const openedMain = await useTabStore
      .getState()
      .openTab(1, 42, "claude_code", true, "Detached")

    expect(openedMain).toBe(false)
    expect(focusDetachedConversation).toHaveBeenCalledWith(42)
    expect(useTabStore.getState().rawTabs).toEqual([])
    expect(useTabStore.getState().activeTabId).toBeNull()
  })

  it("opens a main tab and returns true when focus misses", async () => {
    focusDetachedConversation.mockResolvedValue(false)

    const openedMain = await useTabStore
      .getState()
      .openTab(1, 42, "claude_code", true, "Live")

    expect(openedMain).toBe(true)
    expect(focusDetachedConversation).toHaveBeenCalledWith(42)
    expect(useTabStore.getState().rawTabs).toHaveLength(1)
    expect(useTabStore.getState().rawTabs[0]?.conversationId).toBe(42)
    expect(useTabStore.getState().activeTabId).toBeTruthy()
  })

  it("skips focus gate for non-positive conversation ids (drafts)", async () => {
    const openedMain = await useTabStore
      .getState()
      .openTab(1, 0, "claude_code", true, "Draft")

    expect(openedMain).toBe(true)
    expect(focusDetachedConversation).not.toHaveBeenCalled()
    expect(useTabStore.getState().rawTabs).toHaveLength(1)
  })

  it("does not race: focus success never leaves a main tab behind (cold cache)", async () => {
    // Cold cache: no sync detached flag, but async focus still succeeds.
    let resolveFocus!: (v: boolean) => void
    const focusStarted = new Promise<void>((resolveStarted) => {
      focusDetachedConversation.mockImplementation(
        (_id: number) =>
          new Promise<boolean>((resolve) => {
            resolveFocus = resolve
            resolveStarted()
          })
      )
    })

    const pending = useTabStore
      .getState()
      .openTab(1, 99, "claude_code", true, "Cold")

    await focusStarted
    // While focus is in flight, main tab must not appear yet.
    expect(useTabStore.getState().rawTabs).toEqual([])

    resolveFocus(true)
    await expect(pending).resolves.toBe(false)
    expect(useTabStore.getState().rawTabs).toEqual([])
  })
})
