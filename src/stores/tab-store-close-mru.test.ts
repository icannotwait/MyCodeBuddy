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
  isLocalDesktop: vi.fn(() => true),
}))

vi.mock("@/lib/conversation-popout", () => ({
  focusDetachedConversation: vi.fn(async () => false),
  isPopOutInFlight: vi.fn(() => false),
  isConversationDetachedCache: vi.fn(() => false),
  getTransferEpoch: vi.fn(() => 0),
}))

vi.mock("@/lib/conversation-popout-acp-bridge", () => ({
  isTransferringOut: vi.fn(() => false),
}))

import {
  pickMruTabId,
  resetTabStore,
  useTabStore,
} from "@/stores/tab-store"

describe("closeTab MRU", () => {
  beforeEach(() => {
    resetTabStore()
    useTabStore.setState({
      rawTabs: [
        {
          id: "parent",
          kind: "conversation",
          folderId: 1,
          conversationId: 10,
          agentType: "claude_code",
          title: "Parent",
          isPinned: false,
          activationSeq: 5,
        },
        {
          id: "other",
          kind: "conversation",
          folderId: 1,
          conversationId: 11,
          agentType: "claude_code",
          title: "Other",
          isPinned: false,
          activationSeq: 1,
        },
        {
          id: "child",
          kind: "conversation",
          folderId: 1,
          conversationId: 99,
          agentType: "codex",
          title: "Child",
          isPinned: false,
          activationSeq: 9,
        },
      ],
      activeTabId: "child",
      tabsHydrated: true,
    })
  })

  it("activates highest activationSeq among remaining when closing active tab", () => {
    useTabStore.getState().closeTab("child")
    expect(useTabStore.getState().activeTabId).toBe("parent")
    expect(useTabStore.getState().rawTabs.map((t) => t.id)).toEqual([
      "parent",
      "other",
    ])
  })

  it("falls back to neighbor index when no remaining tab has seq > 0", () => {
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
        {
          id: "b",
          kind: "conversation",
          folderId: 1,
          conversationId: 2,
          agentType: "claude_code",
          title: "B",
          isPinned: false,
        },
        {
          id: "c",
          kind: "conversation",
          folderId: 1,
          conversationId: 3,
          agentType: "claude_code",
          title: "C",
          isPinned: false,
        },
      ],
      activeTabId: "b",
      tabsHydrated: true,
    })
    useTabStore.getState().closeTab("b")
    // index was 1; next = Math.min(1, 1) → second of remaining ["a","c"] → "c"
    expect(useTabStore.getState().activeTabId).toBe("c")
  })

  it("does not change activeTabId when closing a non-active tab", () => {
    useTabStore.getState().closeTab("other")
    expect(useTabStore.getState().activeTabId).toBe("child")
    expect(useTabStore.getState().rawTabs.map((t) => t.id)).toEqual([
      "parent",
      "child",
    ])
  })
})

describe("pickMruTabId", () => {
  it("returns null when all seqs missing or zero", () => {
    expect(
      pickMruTabId([
        { id: "a" },
        { id: "b", activationSeq: 0 },
      ])
    ).toBeNull()
  })

  it("returns id with highest seq", () => {
    expect(
      pickMruTabId([
        { id: "a", activationSeq: 2 },
        { id: "b", activationSeq: 7 },
        { id: "c", activationSeq: 3 },
      ])
    ).toBe("b")
  })
})
