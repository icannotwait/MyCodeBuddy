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

import { pickMruTabId, resetTabStore, useTabStore } from "@/stores/tab-store"

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
      pickMruTabId([{ id: "a" }, { id: "b", activationSeq: 0 }])
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

/**
 * Without stampActiveTab on openTab activation, re-opening parent then child
 * leaves parent at a stale low seq; close(child) would activate "other" (high
 * leftover seq) instead of parent. With stamp, openTab always bumps the newly
 * active tab so parent→child→close returns to parent.
 */
describe("openTab stamps activationSeq for MRU close", () => {
  beforeEach(() => {
    resetTabStore()
  })

  it("parent → other → parent → child → close(child) returns to parent", async () => {
    // Full openTab path: each activation stamps so MRU reflects view order.
    await useTabStore.getState().openTab(1, 10, "claude_code", true, "Parent")
    await useTabStore.getState().openTab(1, 11, "claude_code", true, "Other")
    await useTabStore.getState().openTab(1, 10, "claude_code", true, "Parent")

    const afterParent = useTabStore.getState()
    expect(afterParent.activeTabId).toBe("conv-1-claude_code-10")
    const parentSeq = afterParent.rawTabs.find(
      (t) => t.id === "conv-1-claude_code-10"
    )?.activationSeq
    const otherSeq = afterParent.rawTabs.find(
      (t) => t.id === "conv-1-claude_code-11"
    )?.activationSeq
    expect(parentSeq).toBeGreaterThan(otherSeq ?? 0)

    await useTabStore.getState().openTab(1, 99, "codex", true, "Child")
    const afterChild = useTabStore.getState()
    expect(afterChild.activeTabId).toBe("conv-1-codex-99")
    const childSeq = afterChild.rawTabs.find(
      (t) => t.id === "conv-1-codex-99"
    )?.activationSeq
    const parentSeqAfterChild = afterChild.rawTabs.find(
      (t) => t.id === "conv-1-claude_code-10"
    )?.activationSeq
    expect(childSeq).toBeGreaterThan(parentSeqAfterChild ?? 0)

    useTabStore.getState().closeTab("conv-1-codex-99")
    expect(useTabStore.getState().activeTabId).toBe("conv-1-claude_code-10")
  })

  it("stale high leftover on other loses after openTab re-activates parent then child", async () => {
    // Prove stamp beats a misleading high leftover seq on an unvisited tab.
    useTabStore.setState({
      rawTabs: [
        {
          id: "conv-1-claude_code-10",
          kind: "conversation",
          folderId: 1,
          conversationId: 10,
          agentType: "claude_code",
          title: "Parent",
          isPinned: true,
          activationSeq: 1,
        },
        {
          id: "conv-1-claude_code-11",
          kind: "conversation",
          folderId: 1,
          conversationId: 11,
          agentType: "claude_code",
          title: "Other",
          isPinned: true,
          activationSeq: 100,
        },
      ],
      activeTabId: "conv-1-claude_code-10",
      tabsHydrated: true,
    })

    // Without openTab stamp on re-activate, parent would stay at 1 and lose
    // to other(100) after child close. Stamp parent, then child.
    await useTabStore.getState().openTab(1, 11, "claude_code", true, "Other")
    await useTabStore.getState().openTab(1, 10, "claude_code", true, "Parent")
    await useTabStore.getState().openTab(1, 99, "codex", true, "Child")

    const st = useTabStore.getState()
    const parentSeq = st.rawTabs.find(
      (t) => t.id === "conv-1-claude_code-10"
    )?.activationSeq
    const otherSeq = st.rawTabs.find(
      (t) => t.id === "conv-1-claude_code-11"
    )?.activationSeq
    const childSeq = st.rawTabs.find(
      (t) => t.id === "conv-1-codex-99"
    )?.activationSeq
    expect(parentSeq).toBeGreaterThan(otherSeq ?? 0)
    expect(childSeq).toBeGreaterThan(parentSeq ?? 0)

    useTabStore.getState().closeTab("conv-1-codex-99")
    expect(useTabStore.getState().activeTabId).toBe("conv-1-claude_code-10")
  })

  it("openTab stamps newly appended and activated existing tabs", async () => {
    await useTabStore.getState().openTab(1, 10, "claude_code", true, "Parent")
    await useTabStore.getState().openTab(1, 20, "claude_code", true, "B")
    const tabB = useTabStore
      .getState()
      .rawTabs.find((t) => t.conversationId === 20)
    expect(tabB?.activationSeq).toBeGreaterThan(0)
    expect(useTabStore.getState().activeTabId).toBe(tabB?.id)

    await useTabStore.getState().openTab(1, 10, "claude_code", true, "Parent")
    const parent = useTabStore
      .getState()
      .rawTabs.find((t) => t.conversationId === 10)
    expect(parent?.activationSeq).toBeGreaterThan(tabB?.activationSeq ?? 0)
    expect(useTabStore.getState().activeTabId).toBe(parent?.id)
  })

  it("preview replace openTab stamps the new active tab", async () => {
    await useTabStore
      .getState()
      .openTab(1, 11, "claude_code", true, "PinnedOther")
    // Unpinned preview slot
    await useTabStore.getState().openTab(1, 10, "claude_code", false, "Preview")
    await useTabStore.getState().openTab(1, 99, "codex", false, "Child")

    const child = useTabStore
      .getState()
      .rawTabs.find((t) => t.conversationId === 99)
    const other = useTabStore
      .getState()
      .rawTabs.find((t) => t.conversationId === 11)
    expect(child?.activationSeq).toBeGreaterThan(other?.activationSeq ?? 0)
    expect(useTabStore.getState().activeTabId).toBe(child?.id)

    useTabStore.getState().closeTab(child!.id)
    // Preview was replaced by child; remaining is the pinned other.
    expect(useTabStore.getState().activeTabId).toBe("conv-1-claude_code-11")
  })
})
