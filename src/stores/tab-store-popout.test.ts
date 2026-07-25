import { beforeEach, describe, expect, it, vi } from "vitest"

const focusDetachedConversation = vi.fn(async (_id: number) => false)
const isPopOutInFlight = vi.fn((_id: number) => false)
const isTransferringOut = vi.fn((_id: number | null | undefined) => false)
const isConversationDetachedCache = vi.fn((_id: number) => false)
const transferEpochById = new Map<number, number>()
const getTransferEpoch = vi.fn((id: number) => transferEpochById.get(id) ?? 0)

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
  isPopOutInFlight: (id: number) => isPopOutInFlight(id),
  isConversationDetachedCache: (id: number) => isConversationDetachedCache(id),
  getTransferEpoch: (id: number) => getTransferEpoch(id),
}))

vi.mock("@/lib/conversation-popout-acp-bridge", () => ({
  isTransferringOut: (id: number | null | undefined) => isTransferringOut(id),
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
    isPopOutInFlight.mockReset()
    isPopOutInFlight.mockReturnValue(false)
    isTransferringOut.mockReset()
    isTransferringOut.mockReturnValue(false)
    isConversationDetachedCache.mockReset()
    isConversationDetachedCache.mockReturnValue(false)
    transferEpochById.clear()
    getTransferEpoch.mockClear()
    getTransferEpoch.mockImplementation(
      (id: number) => transferEpochById.get(id) ?? 0
    )
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

  it("returns false / no main tab while transferring fence is set", async () => {
    isTransferringOut.mockReturnValue(true)
    focusDetachedConversation.mockResolvedValue(false)

    const openedMain = await useTabStore
      .getState()
      .openTab(1, 77, "claude_code", true, "DuringTransfer")

    expect(openedMain).toBe(false)
    expect(isTransferringOut).toHaveBeenCalledWith(77)
    expect(useTabStore.getState().rawTabs).toEqual([])
  })

  it("returns false / no main tab while pop-out single-flight is in flight", async () => {
    isPopOutInFlight.mockReturnValue(true)
    focusDetachedConversation.mockResolvedValue(false)

    const openedMain = await useTabStore
      .getState()
      .openTab(1, 88, "claude_code", true, "DuringPopout")

    expect(openedMain).toBe(false)
    expect(isPopOutInFlight).toHaveBeenCalledWith(88)
    expect(useTabStore.getState().rawTabs).toEqual([])
  })

  it("after focus miss, re-checks fence and does not create main tab if transfer ran", async () => {
    let resolveFocus!: (v: boolean) => void
    const focusStarted = new Promise<void>((resolveStarted) => {
      focusDetachedConversation.mockImplementation(
        () =>
          new Promise<boolean>((resolve) => {
            resolveFocus = resolve
            resolveStarted()
          })
      )
    })

    const pending = useTabStore
      .getState()
      .openTab(1, 66, "claude_code", true, "RaceFence")

    await focusStarted
    // Pop-out begins while openTab is suspended on the first focus probe.
    isTransferringOut.mockReturnValue(true)
    resolveFocus(false)

    await expect(pending).resolves.toBe(false)
    expect(useTabStore.getState().rawTabs).toEqual([])
  })

  it("after focus miss, epoch advanced by completed transfer skips main tab", async () => {
    // Race: focus started before pop-out; while pending, pop-out opens,
    // completes, caches detached, and clears fence (epoch start+end).
    // Stale false must not create a main tab beside the detached window.
    let resolveFocus!: (v: boolean) => void
    const focusStarted = new Promise<void>((resolveStarted) => {
      focusDetachedConversation.mockImplementation(
        () =>
          new Promise<boolean>((resolve) => {
            resolveFocus = resolve
            resolveStarted()
          })
      )
    })

    const pending = useTabStore
      .getState()
      .openTab(1, 67, "claude_code", true, "PostPopoutEpoch")

    await focusStarted
    // Full transfer spanned the await: epoch 0 → 1 (start) → 2 (end).
    transferEpochById.set(67, 2)
    isTransferringOut.mockReturnValue(false)
    isPopOutInFlight.mockReturnValue(false)
    isConversationDetachedCache.mockReturnValue(true)
    resolveFocus(false)

    await expect(pending).resolves.toBe(false)
    expect(useTabStore.getState().rawTabs).toEqual([])
  })

  it("stale third-probe-style false after transfer still blocked by epoch", async () => {
    // Former multi-probe race: focus resolves false only after a completed
    // transfer; epoch change alone must prevent main-tab create (no reliance
    // on a later successful probe).
    let resolveFocus!: (v: boolean) => void
    const focusStarted = new Promise<void>((resolveStarted) => {
      focusDetachedConversation.mockImplementation(
        () =>
          new Promise<boolean>((resolve) => {
            resolveFocus = resolve
            resolveStarted()
          })
      )
    })

    const pending = useTabStore
      .getState()
      .openTab(1, 68, "claude_code", true, "StaleEpochMiss")

    await focusStarted
    transferEpochById.set(68, 2)
    isTransferringOut.mockReturnValue(false)
    isPopOutInFlight.mockReturnValue(false)
    // Cache may still be cold if a stale focus wiped it — epoch is the barrier.
    isConversationDetachedCache.mockReturnValue(false)
    resolveFocus(false)

    await expect(pending).resolves.toBe(false)
    expect(useTabStore.getState().rawTabs).toEqual([])
  })

  it("detached cache set after focus miss skips main tab", async () => {
    focusDetachedConversation.mockImplementation(async () => {
      isConversationDetachedCache.mockReturnValue(true)
      return false
    })

    const openedMain = await useTabStore
      .getState()
      .openTab(1, 69, "claude_code", true, "CacheBarrier")

    expect(openedMain).toBe(false)
    expect(useTabStore.getState().rawTabs).toEqual([])
  })
})
