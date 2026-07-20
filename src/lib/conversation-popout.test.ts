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
  return { detachTab, restoreDetachedTab, flushOpenedTabsSave }
})

vi.mock("@/stores/tab-store", () => ({
  useTabStore: {
    getState: () => ({
      rawTabs: [
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
      ],
      detachTab: tabMocks.detachTab,
      restoreDetachedTab: tabMocks.restoreDetachedTab,
      flushOpenedTabsSave: tabMocks.flushOpenedTabsSave,
    }),
  },
}))

import { isLocalDesktop, subscribe } from "@/lib/platform"
import * as api from "@/lib/api"
import {
  canPopOutConversation,
  popOutConversation,
} from "@/lib/conversation-popout"
import {
  __resetTransferFencesForTests,
  isTransferringOut,
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
    vi.mocked(api.abortConversationPopoutOperation).mockClear()
    vi.mocked(api.closeConversationWindow).mockClear()
    vi.mocked(api.getConversationPopoutOperation).mockResolvedValue({
      phase: "aborted",
      conversationId: 1,
      operationId: "op",
    })
  })

  it("aborts then restores when detach CAS fails after ready", async () => {
    tabMocks.flushOpenedTabsSave.mockResolvedValueOnce({
      accepted: false,
      version: 1,
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
})
