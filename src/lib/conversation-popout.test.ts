import { beforeEach, describe, expect, it, vi } from "vitest"

vi.mock("@/lib/platform", () => ({
  isLocalDesktop: vi.fn(() => true),
  subscribe: vi.fn(async () => () => {}),
}))

vi.mock("@/lib/api", () => ({
  focusConversationWindow: vi.fn(async () => false),
  openConversationWindow: vi.fn(async () => "opened"),
  closeConversationWindow: vi.fn(async () => true),
  completeConversationPopoutOperation: vi.fn(async () => ({
    phase: "handoff_complete",
  })),
  abortConversationPopoutOperation: vi.fn(async () => ({})),
  getConversationPopoutOperation: vi.fn(async () => ({
    phase: "aborted",
    conversationId: 1,
    operationId: "x",
  })),
}))

const tabMocks = vi.hoisted(() => {
  const restoreDetachedTab = vi.fn()
  const flushOpenedTabsSave = vi.fn(async () => ({
    accepted: true,
    version: 1,
  }))
  const detachTab = vi.fn(() => ({
    ok: true as const,
    nextActiveId: "b",
    restoreToken: {
      tab: {
        id: "a",
        conversationId: 1,
        folderId: 1,
        agentType: "claude_code" as const,
      },
      index: 0,
      previousActiveTabId: "a",
    },
  }))
  const rawTabs: Array<{
    id: string
    conversationId: number
    folderId: number
    agentType: string
  }> = [
    {
      id: "a",
      conversationId: 1,
      folderId: 1,
      agentType: "claude_code",
    },
    {
      id: "b",
      conversationId: 2,
      folderId: 1,
      agentType: "claude_code",
    },
  ]
  const resetRawTabs = () => {
    rawTabs.splice(
      0,
      rawTabs.length,
      {
        id: "a",
        conversationId: 1,
        folderId: 1,
        agentType: "claude_code",
      },
      {
        id: "b",
        conversationId: 2,
        folderId: 1,
        agentType: "claude_code",
      }
    )
  }
  return { detachTab, restoreDetachedTab, flushOpenedTabsSave, rawTabs, resetRawTabs }
})

vi.mock("@/stores/tab-store", () => ({
  useTabStore: {
    getState: () => ({
      rawTabs: tabMocks.rawTabs,
      detachTab: tabMocks.detachTab,
      restoreDetachedTab: tabMocks.restoreDetachedTab,
      flushOpenedTabsSave: tabMocks.flushOpenedTabsSave,
    }),
  },
}))

import { isLocalDesktop, subscribe } from "@/lib/platform"
import * as api from "@/lib/api"
import {
  __bumpTransferEpochForTests,
  __resetPopoutRuntimeForTests,
  canPopOutConversation,
  focusDetachedConversation,
  getTransferEpoch,
  isConversationDetachedCache,
  isPopOutInFlight,
  popOutConversation,
} from "@/lib/conversation-popout"
import {
  __resetTransferFencesForTests,
  isTransferringOut,
  markMainReleased,
  markTransferringOut,
  registerPopoutAcpBridge,
} from "@/lib/conversation-popout-acp-bridge"

describe("canPopOutConversation", () => {
  beforeEach(() => {
    vi.mocked(isLocalDesktop).mockReturnValue(true)
  })

  it("disables for draft", () => {
    expect(
      canPopOutConversation({
        conversationId: null,
        isOpenMainTab: true,
        mainTabCount: 3,
      })
    ).toEqual({ enabled: false, reason: "draft" })
  })

  it("disables for last main tab", () => {
    expect(
      canPopOutConversation({
        conversationId: 1,
        isOpenMainTab: true,
        mainTabCount: 1,
      })
    ).toEqual({ enabled: false, reason: "last_tab" })
  })

  it("enables when multiple tabs", () => {
    expect(
      canPopOutConversation({
        conversationId: 1,
        isOpenMainTab: true,
        mainTabCount: 2,
      })
    ).toEqual({ enabled: true })
  })

  it("hides for non-local desktop", () => {
    vi.mocked(isLocalDesktop).mockReturnValue(false)
    expect(
      canPopOutConversation({
        conversationId: 1,
        isOpenMainTab: true,
        mainTabCount: 2,
      })
    ).toEqual({ enabled: false, reason: "not_local_desktop" })
  })
})

describe("popOutConversation compensation", () => {
  beforeEach(() => {
    vi.mocked(isLocalDesktop).mockReturnValue(true)
    __resetTransferFencesForTests()
    // Default reclaim no-op so reclaimable compensation can proceed to
    // restore/close; individual tests override or clear the bridge.
    registerPopoutAcpBridge({
      releaseConnectionWithoutDisconnect: () => {},
      reclaimAfterAbort: async () => {},
    })
    tabMocks.resetRawTabs()
    tabMocks.detachTab.mockClear()
    tabMocks.restoreDetachedTab.mockClear()
    tabMocks.flushOpenedTabsSave.mockReset()
    tabMocks.flushOpenedTabsSave.mockResolvedValue({
      accepted: true,
      version: 1,
    })
    tabMocks.detachTab.mockReturnValue({
      ok: true,
      nextActiveId: "b",
      restoreToken: {
        tab: {
          id: "a",
          conversationId: 1,
          folderId: 1,
          agentType: "claude_code",
        },
        index: 0,
        previousActiveTabId: "a",
      },
    })
    vi.mocked(api.focusConversationWindow).mockResolvedValue(false)
    vi.mocked(api.openConversationWindow).mockResolvedValue("opened")
    vi.mocked(api.completeConversationPopoutOperation).mockResolvedValue({
      phase: "handoff_complete",
      conversationId: 1,
      operationId: "op",
    })
    vi.mocked(api.abortConversationPopoutOperation).mockReset()
    vi.mocked(api.abortConversationPopoutOperation).mockResolvedValue({
      never_rebound: null,
    })
    vi.mocked(api.closeConversationWindow).mockClear()
    vi.mocked(api.getConversationPopoutOperation).mockResolvedValue({
      phase: "aborted",
      conversationId: 1,
      operationId: "op",
      abortOutcome: { never_rebound: null },
    })
  })

  it("aborts then restores when detach CAS fails after ready", async () => {
    tabMocks.flushOpenedTabsSave.mockResolvedValueOnce({
      accepted: false,
      version: 1,
    })
    vi.mocked(api.abortConversationPopoutOperation).mockResolvedValue({
      never_rebound: null,
    })

    let readyHandler: ((p: unknown) => void) | null = null
    vi.mocked(subscribe).mockImplementation(async (event, handler) => {
      if (event === "conversation-window://ready") {
        readyHandler = handler as (p: unknown) => void
      }
      return () => {}
    })
    vi.mocked(api.openConversationWindow).mockImplementation(async (args) => {
      queueMicrotask(() => {
        readyHandler?.({
          conversationId: args.conversationId,
          operationId: args.operationId,
        })
      })
      return "opened"
    })

    await expect(
      popOutConversation({
        conversationId: 1,
        folderId: 1,
        agentType: "claude_code",
      })
    ).rejects.toThrow(/CAS rejected|opened_tabs/)

    // reverse/abort before restore
    expect(api.abortConversationPopoutOperation).toHaveBeenCalled()
    expect(tabMocks.restoreDetachedTab).toHaveBeenCalled()
    expect(api.closeConversationWindow).toHaveBeenCalled()
    expect(isTransferringOut(1)).toBe(false)
  })

  it("does not restore or close when status is already handoff_complete", async () => {
    tabMocks.flushOpenedTabsSave.mockResolvedValueOnce({
      accepted: false,
      version: 1,
    })
    vi.mocked(api.getConversationPopoutOperation).mockResolvedValue({
      phase: "handoff_complete",
      conversationId: 1,
      operationId: "op",
    })

    let readyHandler: ((p: unknown) => void) | null = null
    vi.mocked(subscribe).mockImplementation(async (event, handler) => {
      if (event === "conversation-window://ready") {
        readyHandler = handler as (p: unknown) => void
      }
      return () => {}
    })
    vi.mocked(api.openConversationWindow).mockImplementation(async (args) => {
      queueMicrotask(() => {
        readyHandler?.({
          conversationId: args.conversationId,
          operationId: args.operationId,
        })
      })
      return "opened"
    })

    await expect(
      popOutConversation({
        conversationId: 1,
        folderId: 1,
        agentType: "claude_code",
      })
    ).rejects.toThrow()

    expect(api.abortConversationPopoutOperation).toHaveBeenCalled()
    // AlreadyComplete: no restore/close against successful handoff
    expect(tabMocks.restoreDetachedTab).not.toHaveBeenCalled()
    expect(api.closeConversationWindow).not.toHaveBeenCalled()
  })

  it("does not restore or close when abort returns already_complete and status lookup fails", async () => {
    tabMocks.flushOpenedTabsSave.mockResolvedValueOnce({
      accepted: false,
      version: 1,
    })
    vi.mocked(api.abortConversationPopoutOperation).mockResolvedValue({
      already_complete: null,
    })
    vi.mocked(api.getConversationPopoutOperation).mockRejectedValue(
      new Error("status unavailable")
    )

    let readyHandler: ((p: unknown) => void) | null = null
    vi.mocked(subscribe).mockImplementation(async (event, handler) => {
      if (event === "conversation-window://ready") {
        readyHandler = handler as (p: unknown) => void
      }
      return () => {}
    })
    vi.mocked(api.openConversationWindow).mockImplementation(async (args) => {
      queueMicrotask(() => {
        readyHandler?.({
          conversationId: args.conversationId,
          operationId: args.operationId,
        })
      })
      return "opened"
    })

    await expect(
      popOutConversation({
        conversationId: 1,
        folderId: 1,
        agentType: "claude_code",
      })
    ).rejects.toThrow()

    expect(tabMocks.restoreDetachedTab).not.toHaveBeenCalled()
    expect(api.closeConversationWindow).not.toHaveBeenCalled()
  })

  it("reclaims main ACP owner after release when abort is reclaimable", async () => {
    const reclaim = vi.fn(async () => {})
    registerPopoutAcpBridge({
      releaseConnectionWithoutDisconnect: () => {},
      reclaimAfterAbort: reclaim,
    })

    tabMocks.flushOpenedTabsSave.mockResolvedValueOnce({
      accepted: false,
      version: 1,
    })
    vi.mocked(api.abortConversationPopoutOperation).mockResolvedValue({
      reversed: { generation: 2 },
    })

    let readyHandler: ((p: unknown) => void) | null = null
    vi.mocked(subscribe).mockImplementation(async (event, handler) => {
      if (event === "conversation-window://ready") {
        readyHandler = handler as (p: unknown) => void
      }
      return () => {}
    })
    vi.mocked(api.openConversationWindow).mockImplementation(async (args) => {
      queueMicrotask(() => {
        readyHandler?.({
          conversationId: args.conversationId,
          operationId: args.operationId,
        })
      })
      return "opened"
    })

    await expect(
      popOutConversation({
        conversationId: 1,
        folderId: 1,
        agentType: "claude_code",
      })
    ).rejects.toThrow(/CAS rejected|opened_tabs/)

    expect(api.abortConversationPopoutOperation).toHaveBeenCalled()
    // release marks mainReleased before detach CAS fails; reclaim must run
    // with the post-reverse lease (generation + main), not a bare op id.
    expect(reclaim).toHaveBeenCalledWith(
      1,
      expect.any(String),
      expect.objectContaining({
        ownershipGeneration: 2,
        ownerWindowLabel: "main",
      })
    )
    expect(tabMocks.restoreDetachedTab).toHaveBeenCalled()
    expect(api.closeConversationWindow).toHaveBeenCalled()
    expect(isTransferringOut(1)).toBe(false)
  })

  it("does not close detached when reclaim throws / no bridge", async () => {
    // No bridge registered → reclaimAfterAbort fails closed.
    __resetTransferFencesForTests()
    registerPopoutAcpBridge(null)
    tabMocks.flushOpenedTabsSave.mockResolvedValueOnce({
      accepted: false,
      version: 1,
    })
    vi.mocked(api.abortConversationPopoutOperation).mockResolvedValue({
      kind: "reversed",
      generation: 3,
    })

    let readyHandler: ((p: unknown) => void) | null = null
    vi.mocked(subscribe).mockImplementation(async (event, handler) => {
      if (event === "conversation-window://ready") {
        readyHandler = handler as (p: unknown) => void
      }
      return () => {}
    })
    vi.mocked(api.openConversationWindow).mockImplementation(async (args) => {
      queueMicrotask(() => {
        readyHandler?.({
          conversationId: args.conversationId,
          operationId: args.operationId,
        })
      })
      return "opened"
    })

    await expect(
      popOutConversation({
        conversationId: 1,
        folderId: 1,
        agentType: "claude_code",
      })
    ).rejects.toThrow(/reclaim bridge is not registered/i)

    expect(tabMocks.restoreDetachedTab).not.toHaveBeenCalled()
    expect(api.closeConversationWindow).not.toHaveBeenCalled()
  })

  it("retries restore flush up to 3 times and does not close when still rejected", async () => {
    // detach flush rejects once, then every restore flush also rejects
    tabMocks.flushOpenedTabsSave.mockResolvedValue({
      accepted: false,
      version: 9,
    })
    vi.mocked(api.abortConversationPopoutOperation).mockResolvedValue({
      never_rebound: null,
    })

    let readyHandler: ((p: unknown) => void) | null = null
    vi.mocked(subscribe).mockImplementation(async (event, handler) => {
      if (event === "conversation-window://ready") {
        readyHandler = handler as (p: unknown) => void
      }
      return () => {}
    })
    vi.mocked(api.openConversationWindow).mockImplementation(async (args) => {
      queueMicrotask(() => {
        readyHandler?.({
          conversationId: args.conversationId,
          operationId: args.operationId,
        })
      })
      return "opened"
    })

    await expect(
      popOutConversation({
        conversationId: 1,
        folderId: 1,
        agentType: "claude_code",
      })
    ).rejects.toThrow(/CAS rejected after 3 retries|opened_tabs/)

    // 1 detach flush + 3 restore flushes
    expect(tabMocks.flushOpenedTabsSave).toHaveBeenCalledTimes(4)
    expect(tabMocks.restoreDetachedTab).toHaveBeenCalledTimes(3)
    // Must not close detached after restore failure
    expect(api.closeConversationWindow).not.toHaveBeenCalled()
  })

  it("re-resolves current tab id before detach after concurrent openTab", async () => {
    // Snapshot at start has tab "a"; before detach a concurrent open replaces it.
    let readyHandler: ((p: unknown) => void) | null = null
    vi.mocked(subscribe).mockImplementation(async (event, handler) => {
      if (event === "conversation-window://ready") {
        readyHandler = handler as (p: unknown) => void
      }
      return () => {}
    })
    vi.mocked(api.openConversationWindow).mockImplementation(async (args) => {
      // Concurrent openTab race: replace stale main tab with a newly opened one.
      const idx = tabMocks.rawTabs.findIndex((t) => t.id === "a")
      if (idx >= 0) {
        tabMocks.rawTabs[idx] = {
          id: "concurrent-main",
          conversationId: 1,
          folderId: 1,
          agentType: "claude_code",
        }
      }
      queueMicrotask(() => {
        readyHandler?.({
          conversationId: args.conversationId,
          operationId: args.operationId,
        })
      })
      return "opened"
    })

    await popOutConversation({
      conversationId: 1,
      folderId: 1,
      agentType: "claude_code",
    })

    expect(tabMocks.detachTab).toHaveBeenCalledWith("concurrent-main")
  })

  it("connection_gone abort does not reclaim (no invent dead owner)", async () => {
    const reclaim = vi.fn(async () => {})
    registerPopoutAcpBridge({
      releaseConnectionWithoutDisconnect: () => {},
      reclaimAfterAbort: reclaim,
    })
    tabMocks.flushOpenedTabsSave.mockResolvedValueOnce({
      accepted: false,
      version: 1,
    })
    vi.mocked(api.abortConversationPopoutOperation).mockResolvedValue({
      kind: "connection_gone",
    })

    let readyHandler: ((p: unknown) => void) | null = null
    vi.mocked(subscribe).mockImplementation(async (event, handler) => {
      if (event === "conversation-window://ready") {
        readyHandler = handler as (p: unknown) => void
      }
      return () => {}
    })
    vi.mocked(api.openConversationWindow).mockImplementation(async (args) => {
      queueMicrotask(() => {
        readyHandler?.({
          conversationId: args.conversationId,
          operationId: args.operationId,
        })
      })
      return "opened"
    })

    await expect(
      popOutConversation({
        conversationId: 1,
        folderId: 1,
        agentType: "claude_code",
      })
    ).rejects.toThrow(/CAS rejected|opened_tabs/)

    expect(api.abortConversationPopoutOperation).toHaveBeenCalled()
    // Agent gone between forward and abort: never invent CONNECTION_CREATED.
    expect(reclaim).not.toHaveBeenCalled()
    // UI restore still proceeds so the main tab returns.
    expect(tabMocks.restoreDetachedTab).toHaveBeenCalled()
    expect(api.closeConversationWindow).toHaveBeenCalled()
  })

  it("complete rejection after mainReleased still reclaims via compensate", async () => {
    const reclaim = vi.fn(async () => {})
    registerPopoutAcpBridge({
      releaseConnectionWithoutDisconnect: () => {},
      reclaimAfterAbort: reclaim,
    })
    vi.mocked(api.completeConversationPopoutOperation).mockRejectedValue(
      new Error("cannot complete popout operation op while close is reserved")
    )
    vi.mocked(api.abortConversationPopoutOperation).mockResolvedValue({
      kind: "reversed",
      generation: 8,
    })

    let readyHandler: ((p: unknown) => void) | null = null
    vi.mocked(subscribe).mockImplementation(async (event, handler) => {
      if (event === "conversation-window://ready") {
        readyHandler = handler as (p: unknown) => void
      }
      return () => {}
    })
    vi.mocked(api.openConversationWindow).mockImplementation(async (args) => {
      queueMicrotask(() => {
        readyHandler?.({
          conversationId: args.conversationId,
          operationId: args.operationId,
        })
      })
      return "opened"
    })

    await expect(
      popOutConversation({
        conversationId: 1,
        folderId: 1,
        agentType: "claude_code",
      })
    ).rejects.toThrow(/close is reserved|complete/)

    expect(reclaim).toHaveBeenCalledWith(
      1,
      expect.any(String),
      expect.objectContaining({
        ownershipGeneration: 8,
        ownerWindowLabel: "main",
      })
    )
    expect(tabMocks.restoreDetachedTab).toHaveBeenCalled()
  })

  it("pre-ready reverse refreshes main lease when main never released", async () => {
    // Ready never arrives; closed fires after open. Main still holds the
    // connection (no release). Reverse abort must still refresh the lease.
    const reclaim = vi.fn(async () => {})
    registerPopoutAcpBridge({
      releaseConnectionWithoutDisconnect: () => {},
      reclaimAfterAbort: reclaim,
    })
    vi.mocked(api.abortConversationPopoutOperation).mockResolvedValue({
      kind: "reversed",
      generation: 4,
    })

    let closedHandler: ((p: unknown) => void) | null = null
    vi.mocked(subscribe).mockImplementation(async (event, handler) => {
      if (event === "conversation-window://closed") {
        closedHandler = handler as (p: unknown) => void
      }
      return () => {}
    })
    vi.mocked(api.openConversationWindow).mockImplementation(async (args) => {
      queueMicrotask(() => {
        closedHandler?.({
          conversationId: args.conversationId,
          operationId: args.operationId,
        })
      })
      return "opened"
    })

    await expect(
      popOutConversation({
        conversationId: 1,
        folderId: 1,
        agentType: "claude_code",
      })
    ).rejects.toThrow(/closed before handoff|timed out/)

    expect(reclaim).toHaveBeenCalledWith(
      1,
      expect.any(String),
      expect.objectContaining({
        ownershipGeneration: 4,
        ownerWindowLabel: "main",
      })
    )
  })
})

describe("isPopOutInFlight / transfer fence for openTab", () => {
  beforeEach(() => {
    __resetTransferFencesForTests()
    __resetPopoutRuntimeForTests()
  })

  it("reports transferring fence for openTab skip", () => {
    markTransferringOut(55, "op-fence")
    expect(isTransferringOut(55)).toBe(true)
    markMainReleased(55, "op-fence")
    expect(isTransferringOut(55)).toBe(true)
  })

  it("isPopOutInFlight is false when idle", () => {
    expect(isPopOutInFlight(1)).toBe(false)
  })

  it("bumps transfer epoch at pop-out start and end", async () => {
    const before = getTransferEpoch(1)
    let readyHandler: ((p: unknown) => void) | null = null
    vi.mocked(subscribe).mockImplementation(async (event, handler) => {
      if (event === "conversation-window://ready") {
        readyHandler = handler as (p: unknown) => void
      }
      return () => {}
    })
    vi.mocked(api.openConversationWindow).mockImplementation(async (args) => {
      expect(getTransferEpoch(1)).toBe(before + 1)
      queueMicrotask(() => {
        readyHandler?.({
          conversationId: args.conversationId,
          operationId: args.operationId,
        })
      })
      return "opened"
    })

    await popOutConversation({
      conversationId: 1,
      folderId: 1,
      agentType: "claude_code",
    })

    expect(getTransferEpoch(1)).toBe(before + 2)
  })
})

describe("focusDetachedConversation cache CAS", () => {
  beforeEach(() => {
    __resetTransferFencesForTests()
    __resetPopoutRuntimeForTests()
    vi.mocked(api.focusConversationWindow).mockReset()
  })

  it("deletes cache on miss when epoch is unchanged", async () => {
    vi.mocked(api.focusConversationWindow).mockResolvedValueOnce(true)
    await focusDetachedConversation(7)
    expect(isConversationDetachedCache(7)).toBe(true)

    vi.mocked(api.focusConversationWindow).mockResolvedValueOnce(false)
    await expect(focusDetachedConversation(7)).resolves.toBe(false)
    expect(isConversationDetachedCache(7)).toBe(false)
  })

  it("does not wipe cache when epoch advances during a stale miss", async () => {
    vi.mocked(api.focusConversationWindow).mockResolvedValueOnce(true)
    await focusDetachedConversation(7)
    expect(isConversationDetachedCache(7)).toBe(true)

    let resolveMiss!: (v: boolean) => void
    let started!: () => void
    const gate = new Promise<void>((r) => {
      started = r
    })
    vi.mocked(api.focusConversationWindow).mockImplementation(
      () =>
        new Promise<boolean>((resolve) => {
          resolveMiss = resolve
          started()
        })
    )

    const pending = focusDetachedConversation(7)
    await gate
    // Concurrent pop-out start/end while stale focus is in flight.
    __bumpTransferEpochForTests(7)
    __bumpTransferEpochForTests(7)
    resolveMiss(false)

    await expect(pending).resolves.toBe(false)
    // Stale false must not delete the concurrent successful pop-out cache.
    expect(isConversationDetachedCache(7)).toBe(true)
  })
})
