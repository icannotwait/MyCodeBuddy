import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

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

import { listOpenedTabs, saveOpenedTabs } from "@/lib/api"
import type { OpenedTab } from "@/lib/types"
import {
  MAX_MAIN_CONVERSATION_TABS,
  evictTabsToLimit,
  buildTabLimitKeepIds,
  resetTabStore,
  useTabStore,
  type DetachRestoreToken,
  type TabItemInternal,
} from "@/stores/tab-store"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"

function openedItem(
  conversationId: number,
  opts: {
    is_active?: boolean
    is_pinned?: boolean
    folder_id?: number
  } = {}
): OpenedTab {
  return {
    id: conversationId,
    folder_id: opts.folder_id ?? 1,
    conversation_id: conversationId,
    agent_type: "claude_code",
    position: conversationId - 1,
    is_active: opts.is_active ?? false,
    is_pinned: opts.is_pinned ?? true,
  }
}

async function waitTabsHydrated(maxTicks = 40) {
  for (let i = 0; i < maxTicks; i++) {
    if (useTabStore.getState().tabsHydrated) return
    await Promise.resolve()
  }
  throw new Error("tabsHydrated never became true")
}

function makeTab(
  id: string,
  overrides: Partial<{
    conversationId: number | null
    activationSeq: number
    isPinned: boolean
    folderId: number
  }> = {}
) {
  return {
    id,
    kind: "conversation" as const,
    folderId: overrides.folderId ?? 1,
    conversationId:
      overrides.conversationId === undefined ? 1 : overrides.conversationId,
    agentType: "claude_code" as const,
    title: id,
    isPinned: overrides.isPinned ?? false,
    activationSeq: overrides.activationSeq,
  }
}

/** Seed N conversation tabs with activationSeq 1..N (or custom). */
function seedConversationTabs(
  n: number,
  options: {
    allPinned?: boolean
    activationSeqFor?: (i: number) => number | undefined
  } = {}
): TabItemInternal[] {
  const tabs: TabItemInternal[] = Array.from({ length: n }, (_, i) => {
    const convId = i + 1
    const seq = options.activationSeqFor
      ? options.activationSeqFor(i)
      : i + 1
    return {
      id: `conv-1-claude_code-${convId}`,
      kind: "conversation" as const,
      folderId: 1,
      conversationId: convId,
      agentType: "claude_code" as const,
      title: `t${i}`,
      isPinned: options.allPinned ?? true,
      ...(seq !== undefined ? { activationSeq: seq } : {}),
    }
  })
  useTabStore.setState({
    rawTabs: tabs,
    activeTabId: tabs[tabs.length - 1]?.id ?? null,
    tabsHydrated: true,
    previewReplacedTabIds: [],
  })
  return tabs
}

describe("evictTabsToLimit", () => {
  it("evicts lowest activationSeq until length <= MAX", () => {
    const tabs = Array.from({ length: 11 }, (_, i) =>
      makeTab(`t${i}`, {
        conversationId: i + 1,
        activationSeq: i + 1,
      })
    )
    const { tabs: next, evictedIds } = evictTabsToLimit(tabs, {
      keepTabIds: ["t10"],
    })
    expect(next).toHaveLength(MAX_MAIN_CONVERSATION_TABS)
    expect(evictedIds).toEqual(["t0"])
    expect(next.map((t) => t.id)).not.toContain("t0")
    expect(next.map((t) => t.id)).toContain("t10")
  })

  it("never removes keepTabIds or drafts", () => {
    const tabs = [
      makeTab("draft", { conversationId: null, activationSeq: 0 }),
      ...Array.from({ length: 10 }, (_, i) =>
        makeTab(`t${i}`, { conversationId: i + 1, activationSeq: i + 1 })
      ),
    ]
    const keep = buildTabLimitKeepIds(tabs, ["t9"])
    const { tabs: next } = evictTabsToLimit(tabs, { keepTabIds: keep })
    expect(next.find((t) => t.id === "draft")).toBeTruthy()
    expect(next.find((t) => t.id === "t9")).toBeTruthy()
    expect(next).toHaveLength(10)
  })

  it("evicts pinned LRU victim", () => {
    const tabs = Array.from({ length: 11 }, (_, i) =>
      makeTab(`t${i}`, {
        conversationId: i + 1,
        activationSeq: i === 0 ? 1 : i + 10,
        isPinned: i === 0,
      })
    )
    const { evictedIds } = evictTabsToLimit(tabs, { keepTabIds: ["t10"] })
    expect(evictedIds).toEqual(["t0"])
  })

  it("tie-breaks equal seq by lower index re-evaluated after each removal", () => {
    const tabs = Array.from({ length: 13 }, (_, i) =>
      makeTab(`t${i}`, { conversationId: i + 1 })
    )
    const { tabs: next, evictedIds } = evictTabsToLimit(tabs, {
      keepTabIds: ["t12"],
    })
    expect(next).toHaveLength(10)
    expect(evictedIds).toEqual(["t0", "t1", "t2"])
  })
})

describe("local open paths under limit", () => {
  beforeEach(() => {
    resetTabStore()
    useTabStore.setState({ tabsHydrated: true })
  })

  it("openTab pin append opens 11th and evicts lowest-seq", async () => {
    seedConversationTabs(10)
    const opened = await useTabStore
      .getState()
      .openTab(1, 100, "claude_code", true, "New")
    expect(opened).toBe(true)

    const st = useTabStore.getState()
    expect(st.rawTabs).toHaveLength(MAX_MAIN_CONVERSATION_TABS)
    expect(st.rawTabs.map((t) => t.id)).not.toContain("conv-1-claude_code-1")
    expect(st.activeTabId).toBe("conv-1-claude_code-100")
    expect(
      st.rawTabs.find((t) => t.id === st.activeTabId)?.activationSeq
    ).toBeGreaterThan(0)
  })

  it("preview replace does not change length", async () => {
    // 9 pinned + 1 unpinned preview slot
    const tabs = seedConversationTabs(10, { allPinned: true })
    tabs[9] = { ...tabs[9], isPinned: false }
    useTabStore.setState({ rawTabs: tabs, activeTabId: tabs[9].id })

    const beforeIds = tabs.map((t) => t.id)
    await useTabStore.getState().openTab(1, 200, "claude_code", false, "Preview")

    const st = useTabStore.getState()
    expect(st.rawTabs).toHaveLength(10)
    // High-seq early tabs still present (no eviction of the pinned working set)
    for (let i = 0; i < 9; i++) {
      expect(st.rawTabs.map((t) => t.id)).toContain(beforeIds[i])
    }
    expect(st.rawTabs.find((t) => t.conversationId === 200)).toBeTruthy()
    expect(st.rawTabs.find((t) => t.conversationId === 10)).toBeUndefined()
  })

  it("raising seq then open keeps warm tab", async () => {
    seedConversationTabs(10)
    // Early tab (lowest original seq) becomes warm via switchTab
    useTabStore.getState().switchTab("conv-1-claude_code-1")
    const warmSeq = useTabStore
      .getState()
      .rawTabs.find((t) => t.id === "conv-1-claude_code-1")?.activationSeq
    expect(warmSeq).toBeGreaterThan(10)

    await useTabStore
      .getState()
      .openTab(1, 100, "claude_code", true, "New")

    const st = useTabStore.getState()
    expect(st.rawTabs).toHaveLength(10)
    expect(st.rawTabs.map((t) => t.id)).toContain("conv-1-claude_code-1")
    // Coldest remaining among non-warm non-new should be gone (seq 2)
    expect(st.rawTabs.map((t) => t.id)).not.toContain("conv-1-claude_code-2")
    expect(st.activeTabId).toBe("conv-1-claude_code-100")
  })

  it("openTab pin=false when all pinned appends and evicts", async () => {
    seedConversationTabs(10, { allPinned: true })
    await useTabStore
      .getState()
      .openTab(1, 100, "claude_code", false, "AllPinnedAppend")

    const st = useTabStore.getState()
    expect(st.rawTabs.length).toBeLessThanOrEqual(MAX_MAIN_CONVERSATION_TABS)
    expect(st.rawTabs).toHaveLength(10)
    expect(st.rawTabs.map((t) => t.id)).not.toContain("conv-1-claude_code-1")
    expect(st.activeTabId).toBe("conv-1-claude_code-100")
  })

  it("openNewConversationTab at capacity creates stamped draft and length ≤10", () => {
    seedConversationTabs(10)
    useTabStore.getState().openNewConversationTab(1, "/tmp/proj")

    const st = useTabStore.getState()
    expect(st.rawTabs.length).toBeLessThanOrEqual(MAX_MAIN_CONVERSATION_TABS)
    const draft = st.rawTabs.find((t) => t.conversationId == null)
    expect(draft).toBeTruthy()
    expect(st.activeTabId).toBe(draft!.id)
    expect(draft!.activationSeq).toBeGreaterThan(0)
  })

  it("openChatModeTab at capacity creates stamped chat draft and length ≤10", () => {
    seedConversationTabs(10)
    useTabStore.getState().openChatModeTab()

    const st = useTabStore.getState()
    expect(st.rawTabs.length).toBeLessThanOrEqual(MAX_MAIN_CONVERSATION_TABS)
    const draft = st.rawTabs.find((t) => t.conversationId == null)
    expect(draft).toBeTruthy()
    expect(draft!.isChat).toBe(true)
    expect(st.activeTabId).toBe(draft!.id)
    expect(draft!.activationSeq).toBeGreaterThan(0)
  })

  it("restoreDetachedTab insert at capacity keeps restored active; sameConv does not shrink", () => {
    seedConversationTabs(10)
    const token: DetachRestoreToken = {
      tab: {
        id: "restored-x",
        kind: "conversation",
        folderId: 1,
        conversationId: 999,
        agentType: "claude_code",
        title: "Restored",
        isPinned: true,
        activationSeq: 50,
      },
      index: 5,
      previousActiveTabId: "conv-1-claude_code-10",
    }

    useTabStore.getState().restoreDetachedTab(token)
    let st = useTabStore.getState()
    expect(st.rawTabs.length).toBeLessThanOrEqual(MAX_MAIN_CONVERSATION_TABS)
    expect(st.rawTabs).toHaveLength(10)
    expect(st.activeTabId).toBe("restored-x")
    expect(st.rawTabs.map((t) => t.id)).toContain("restored-x")
    expect(st.rawTabs.map((t) => t.id)).not.toContain("conv-1-claude_code-1")

    // sameConv: restore an already-present conversation — activate only
    const beforeLen = st.rawTabs.length
    const sameToken: DetachRestoreToken = {
      tab: {
        id: "different-id-same-conv",
        kind: "conversation",
        folderId: 1,
        conversationId: 5,
        agentType: "claude_code",
        title: "SameConv",
        isPinned: true,
      },
      index: 0,
      previousActiveTabId: st.activeTabId,
    }
    useTabStore.getState().restoreDetachedTab(sameToken)
    st = useTabStore.getState()
    expect(st.rawTabs).toHaveLength(beforeLen)
    expect(st.activeTabId).toBe("conv-1-claude_code-5")
    expect(st.rawTabs.map((t) => t.id)).not.toContain("different-id-same-conv")
  })

  it("pinned LRU victim is removed", async () => {
    seedConversationTabs(10, {
      allPinned: true,
      activationSeqFor: (i) => (i === 0 ? 1 : i + 10),
    })
    await useTabStore
      .getState()
      .openTab(1, 100, "claude_code", true, "New")

    const st = useTabStore.getState()
    expect(st.rawTabs.map((t) => t.id)).not.toContain("conv-1-claude_code-1")
    expect(st.rawTabs).toHaveLength(10)
  })

  it("eviction does not append previewReplacedTabIds or call acpDisconnect", async () => {
    const acpDisconnect = vi.fn()
    useTabStore.getState().setSideEffects({
      activateConversationPane: () => {},
      acpDisconnect,
    })
    seedConversationTabs(10, { allPinned: true })
    useTabStore.setState({ previewReplacedTabIds: [] })

    await useTabStore
      .getState()
      .openTab(1, 100, "claude_code", true, "New")

    expect(useTabStore.getState().previewReplacedTabIds).toEqual([])
    expect(acpDisconnect).not.toHaveBeenCalled()
    expect(useTabStore.getState().rawTabs).toHaveLength(10)
  })

  it("closeOtherTabs / closeTabsByFolder stamp the new active", () => {
    seedConversationTabs(5)
    // closeOtherTabs keeps "conv-1-claude_code-2" and makes it active
    const keepId = "conv-1-claude_code-2"
    const beforeSeq =
      useTabStore.getState().rawTabs.find((t) => t.id === keepId)
        ?.activationSeq ?? 0
    useTabStore.getState().closeOtherTabs(keepId)
    const afterOther = useTabStore.getState()
    expect(afterOther.activeTabId).toBe(keepId)
    expect(
      afterOther.rawTabs.find((t) => t.id === keepId)?.activationSeq
    ).toBeGreaterThan(beforeSeq)

    // closeTabsByFolder: fall back to another survivor and stamp it
    useTabStore.setState({
      rawTabs: [
        makeTab("f1-a", {
          folderId: 1,
          conversationId: 1,
          activationSeq: 1,
        }),
        makeTab("f1-b", {
          folderId: 1,
          conversationId: 2,
          activationSeq: 2,
        }),
        makeTab("f2-a", {
          folderId: 2,
          conversationId: 3,
          activationSeq: 3,
        }),
      ],
      activeTabId: "f1-a",
      tabsHydrated: true,
    })
    const f2Before =
      useTabStore.getState().rawTabs.find((t) => t.id === "f2-a")
        ?.activationSeq ?? 0
    useTabStore.getState().closeTabsByFolder(1)
    const afterFolder = useTabStore.getState()
    expect(afterFolder.activeTabId).toBe("f2-a")
    expect(
      afterFolder.rawTabs.find((t) => t.id === "f2-a")?.activationSeq
    ).toBeGreaterThan(f2Before)
  })

  it("defensive stop: keep set covers all tabs → no deletion", () => {
    const tabs = Array.from({ length: 12 }, (_, i) =>
      makeTab(`t${i}`, { conversationId: null })
    )
    const keep = new Set(tabs.map((t) => t.id))
    const { tabs: next, evictedIds } = evictTabsToLimit(tabs, {
      keepTabIds: keep,
    })
    expect(next).toHaveLength(12)
    expect(evictedIds).toEqual([])
  })

  it("draft reuse path stamps the existing draft", () => {
    useTabStore.setState({
      rawTabs: [
        makeTab("draft-1", {
          conversationId: null,
          activationSeq: 1,
          isPinned: true,
        }),
        makeTab("conv-a", { conversationId: 5, activationSeq: 2 }),
      ],
      activeTabId: "conv-a",
      tabsHydrated: true,
    })
    // No folder/agent change → just focus the draft
    useTabStore.getState().openNewConversationTab(1, "")
    const st = useTabStore.getState()
    expect(st.activeTabId).toBe("draft-1")
    expect(
      st.rawTabs.find((t) => t.id === "draft-1")?.activationSeq
    ).toBeGreaterThan(1)
  })

  it("closeAllTabs stamps the replacement draft", () => {
    // Seed a folder so closeAllTabs spawns a replacement draft instead of empty.
    useAppWorkspaceStore.setState({
      folders: [
        {
          id: 1,
          name: "proj",
          path: "/tmp/proj",
          kind: "project",
        },
      ] as never,
      allFolders: [
        {
          id: 1,
          name: "proj",
          path: "/tmp/proj",
          kind: "project",
        },
      ] as never,
    })

    seedConversationTabs(3)
    useTabStore.getState().closeAllTabs()
    const st = useTabStore.getState()
    expect(st.rawTabs).toHaveLength(1)
    expect(st.rawTabs[0].conversationId).toBeNull()
    expect(st.activeTabId).toBe(st.rawTabs[0].id)
    expect(st.rawTabs[0].activationSeq).toBeGreaterThan(0)
  })
})

describe("hydrate / remote over limit", () => {
  beforeEach(() => {
    resetTabStore()
    vi.useFakeTimers()
    vi.mocked(listOpenedTabs).mockReset()
    vi.mocked(saveOpenedTabs).mockReset()
    vi.mocked(saveOpenedTabs).mockResolvedValue({
      accepted: true,
      version: 1,
      tabs: [],
    })
    useAppWorkspaceStore.setState({
      folders: [
        {
          id: 1,
          name: "proj",
          path: "/tmp/proj",
          kind: "project",
        },
      ] as never,
      allFolders: [
        {
          id: 1,
          name: "proj",
          path: "/tmp/proj",
          kind: "project",
        },
      ] as never,
    })
  })

  afterEach(() => {
    vi.useRealTimers()
  })

  it("hydrate 11 items keeps active and saves survivors once", async () => {
    const items = Array.from({ length: 11 }, (_, i) =>
      openedItem(i + 1, { is_active: i === 10 })
    )
    vi.mocked(listOpenedTabs).mockResolvedValue({
      version: 3,
      items,
    })

    const unsub = useTabStore.getState().hydrate()
    await waitTabsHydrated()

    expect(useTabStore.getState().tabsHydrated).toBe(true)
    expect(useTabStore.getState().rawTabs).toHaveLength(
      MAX_MAIN_CONVERSATION_TABS
    )
    expect(useTabStore.getState().activeTabId).toBe(
      "conv-1-claude_code-11"
    )
    expect(
      useTabStore.getState().rawTabs.map((t) => t.id)
    ).not.toContain("conv-1-claude_code-1")

    // Drive save after hydrated (store unit tests have no React effect).
    // Hydrate may already have armed a save; either path must yield exactly one CAS.
    useTabStore.getState().runSaveEffect()
    await vi.advanceTimersByTimeAsync(500)

    expect(saveOpenedTabs).toHaveBeenCalledTimes(1)
    const [savedItems, expectedVersion] = vi.mocked(saveOpenedTabs).mock
      .calls[0]
    expect(savedItems).toHaveLength(10)
    expect(expectedVersion).toBe(3)
    unsub()
  })

  it("handleTabsChanged 11 tabs preserves activationSeq and saves once after eviction", async () => {
    // Seed version + hydrated via a ≤10 hydrate first.
    const seedItems = Array.from({ length: 10 }, (_, i) =>
      openedItem(i + 1, { is_active: i === 9 })
    )
    vi.mocked(listOpenedTabs).mockResolvedValue({
      version: 1,
      items: seedItems,
    })
    const unsub = useTabStore.getState().hydrate()
    await waitTabsHydrated()
    unsub()

    // Stamp local recency so remote match can preserve it.
    const stampedLocal = useTabStore.getState().rawTabs.map((t, i) => ({
      ...t,
      // conv 1 is warm (high seq); others low
      activationSeq: t.conversationId === 1 ? 100 : i + 1,
    }))
    useTabStore.setState({
      rawTabs: stampedLocal,
      activeTabId: "conv-1-claude_code-10",
    })

    vi.mocked(saveOpenedTabs).mockClear()

    const remoteTabs = Array.from({ length: 11 }, (_, i) =>
      openedItem(i + 1, { is_active: i === 10 })
    )
    useTabStore.getState().handleTabsChanged({
      version: 2,
      origin: "remote",
      tabs: remoteTabs,
    })

    const st = useTabStore.getState()
    expect(st.rawTabs.length).toBeLessThanOrEqual(MAX_MAIN_CONVERSATION_TABS)
    expect(st.rawTabs).toHaveLength(10)
    // Matched warm local tab keeps prior activationSeq
    expect(
      st.rawTabs.find((t) => t.conversationId === 1)?.activationSeq
    ).toBe(100)
    // New remote active is present and stamped
    expect(st.activeTabId).toBe("conv-1-claude_code-11")
    expect(
      st.rawTabs.find((t) => t.id === st.activeTabId)?.activationSeq
    ).toBeGreaterThan(0)

    // applyRemoteSnapshot double-runSaveEffect should already arm save; re-drive is ok
    useTabStore.getState().runSaveEffect()
    await vi.advanceTimersByTimeAsync(500)

    expect(saveOpenedTabs).toHaveBeenCalledTimes(1)
    const [savedItems, expectedVersion] = vi.mocked(saveOpenedTabs).mock
      .calls[0]
    expect(savedItems).toHaveLength(10)
    expect(expectedVersion).toBe(2)
  })

  it("local draft + remote 10 keeps draft", async () => {
    vi.mocked(listOpenedTabs).mockResolvedValue({
      version: 1,
      items: [openedItem(1, { is_active: true })],
    })
    const unsub = useTabStore.getState().hydrate()
    await waitTabsHydrated()
    unsub()

    // Replace with a local draft as active focus.
    useTabStore.setState({
      rawTabs: [
        makeTab("draft-local", {
          conversationId: null,
          folderId: 1,
          activationSeq: 50,
        }),
      ],
      activeTabId: "draft-local",
      tabsHydrated: true,
    })

    vi.mocked(saveOpenedTabs).mockClear()

    const remoteTabs = Array.from({ length: 10 }, (_, i) =>
      openedItem(i + 1, { is_active: i === 0 })
    )
    useTabStore.getState().handleTabsChanged({
      version: 2,
      origin: "remote",
      tabs: remoteTabs,
    })

    const st = useTabStore.getState()
    const draft = st.rawTabs.find((t) => t.conversationId == null)
    expect(draft).toBeTruthy()
    expect(draft!.id).toBe("draft-local")
    expect(st.rawTabs.length).toBeLessThanOrEqual(MAX_MAIN_CONVERSATION_TABS)
    // 10 remote + draft → 11 → evict one conversation; draft stays
    expect(st.rawTabs).toHaveLength(10)
    expect(st.activeTabId).toBe("draft-local")

    useTabStore.getState().runSaveEffect()
    await vi.advanceTimersByTimeAsync(500)
    expect(saveOpenedTabs).toHaveBeenCalledTimes(1)
    const [savedItems] = vi.mocked(saveOpenedTabs).mock.calls[0]
    // Persist payload is conversation-bound only (draft device-local)
    expect(savedItems).toHaveLength(9)
  })

  it("hydrate multi-evict 13→10 left-to-right when all seq missing", async () => {
    const items = Array.from({ length: 13 }, (_, i) =>
      openedItem(i + 1, { is_active: i === 12 })
    )
    vi.mocked(listOpenedTabs).mockResolvedValue({
      version: 5,
      items,
    })

    const unsub = useTabStore.getState().hydrate()
    await waitTabsHydrated()

    const st = useTabStore.getState()
    expect(st.rawTabs).toHaveLength(10)
    // Leftmost non-kept (no seq) victims: conv 1,2,3; active 13 kept
    expect(st.rawTabs.map((t) => t.id)).not.toContain("conv-1-claude_code-1")
    expect(st.rawTabs.map((t) => t.id)).not.toContain("conv-1-claude_code-2")
    expect(st.rawTabs.map((t) => t.id)).not.toContain("conv-1-claude_code-3")
    expect(st.activeTabId).toBe("conv-1-claude_code-13")
    expect(st.rawTabs.map((t) => t.id)).toContain("conv-1-claude_code-13")
    unsub()
  })
})
