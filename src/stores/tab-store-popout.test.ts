import { beforeEach, describe, expect, it, vi } from "vitest"

const focusDetachedConversation = vi.fn(async (_id: number) => false)
const isPopOutInFlight = vi.fn((_id: number) => false)
const isTransferringOut = vi.fn((_id: number | null | undefined) => false)
const isConversationDetachedCache = vi.fn((_id: number) => false)

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
  isConversationDetachedCache: (id: number) =>
    isConversationDetachedCache(id),
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

  it("after focus miss and fence clear, second focus success skips main tab", async () => {
    // Barrier: first focus resolves false only after pop-out finished and
    // cleared its fence; a second probe must still find the detached window.
    let call = 0
    focusDetachedConversation.mockImplementation(async () => {
      call += 1
      if (call === 1) {
        // Simulate: while awaiting, pop-out completes and clears the fence
        // but leaves the detached window open.
        isTransferringOut.mockReturnValue(false)
        isPopOutInFlight.mockReturnValue(false)
        return false
      }
      return true
    })

    const openedMain = await useTabStore
      .getState()
      .openTab(1, 67, "claude_code", true, "PostPopoutFocus")

    expect(openedMain).toBe(false)
    expect(focusDetachedConversation).toHaveBeenCalledTimes(2)
    expect(useTabStore.getState().rawTabs).toEqual([])
  })

  it("second focus false after transfer complete still rechecks cache/third focus before create", async () => {
    // Race: first and second focus probes began before the detached window
    // existed; both resolve false only after pop-out finished and cleared the
    // fence. openTab must not create a main tab from those stale misses.
    let call = 0
    let resolveFirst!: (v: boolean) => void
    let resolveSecond!: (v: boolean) => void
    focusDetachedConversation.mockImplementation(
      () =>
        new Promise<boolean>((resolve) => {
          call += 1
          if (call === 1) {
            resolveFirst = resolve
          } else if (call === 2) {
            resolveSecond = resolve
          } else {
            // Third probe after barrier recheck — detached now focused.
            resolve(true)
          }
        })
    )

    const pending = useTabStore
      .getState()
      .openTab(1, 68, "claude_code", true, "StaleDoubleMiss")

    // Let the first focus suspend.
    await vi.waitFor(() => expect(resolveFirst).toBeTypeOf("function"))
    // Transfer completes while first probe is pending.
    isTransferringOut.mockReturnValue(false)
    isPopOutInFlight.mockReturnValue(false)
    isConversationDetachedCache.mockReturnValue(false)
    resolveFirst(false)

    await vi.waitFor(() => expect(resolveSecond).toBeTypeOf("function"))
    // Second probe also resolves false after transfer (stale), but cache
    // may still be cold; third focus must catch the detached window.
    resolveSecond(false)

    await expect(pending).resolves.toBe(false)
    expect(focusDetachedConversation).toHaveBeenCalledTimes(3)
    expect(useTabStore.getState().rawTabs).toEqual([])
  })

  it("second focus false + detached cache set skips main tab without relying on third focus alone", async () => {
    let call = 0
    focusDetachedConversation.mockImplementation(async () => {
      call += 1
      if (call === 1) {
        isTransferringOut.mockReturnValue(false)
        isPopOutInFlight.mockReturnValue(false)
        return false
      }
      if (call === 2) {
        // Transfer finished; detached is known via cache even if focus lags.
        isConversationDetachedCache.mockReturnValue(true)
        return false
      }
      return false
    })

    const openedMain = await useTabStore
      .getState()
      .openTab(1, 69, "claude_code", true, "CacheBarrier")

    expect(openedMain).toBe(false)
    expect(useTabStore.getState().rawTabs).toEqual([])
    // Stops after second miss + cache check (no main mutation).
    expect(focusDetachedConversation.mock.calls.length).toBeGreaterThanOrEqual(
      2
    )
  })
})
