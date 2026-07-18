import { beforeEach, describe, expect, it, vi } from "vitest"

vi.mock("@/lib/api", () => ({
  listOpenedTabs: vi.fn(async () => []),
  saveOpenedTabs: vi.fn(async () => ({ ok: true })),
  getFolderConversation: vi.fn(),
}))

vi.mock("@/lib/platform", () => ({
  subscribe: vi.fn(async () => () => {}),
  onTransportReconnect: vi.fn(() => () => {}),
}))

import {
  resetTabStore,
  useTabStore,
  type TabItemInternal,
} from "@/stores/tab-store"

function draftTab(overrides: Partial<TabItemInternal> = {}): TabItemInternal {
  return {
    id: "new-1",
    conversationId: null,
    agentType: "codex",
    title: "New",
    folderId: 1,
    workingDir: "/repo",
    isChat: false,
    ...overrides,
  }
}

describe("tab-store draft delegation route override", () => {
  beforeEach(() => {
    resetTabStore()
  })

  it("setDraftDelegationRoute stores memory-only override on drafts", () => {
    useTabStore.setState({
      rawTabs: [draftTab()],
      activeTabId: "new-1",
    })
    useTabStore.getState().setDraftDelegationRoute("new-1", "native")
    const tab = useTabStore.getState().rawTabs.find((t) => t.id === "new-1")
    expect(tab?.delegationRouteOverride).toBe("native")

    // Idempotent when unchanged.
    const before = useTabStore.getState().rawTabs
    useTabStore.getState().setDraftDelegationRoute("new-1", "native")
    expect(useTabStore.getState().rawTabs).toBe(before)
  })

  it("setDraftDelegationRoute ignores bound (non-draft) tabs", () => {
    useTabStore.setState({
      rawTabs: [
        draftTab({
          id: "bound-1",
          conversationId: 42,
          title: "Bound",
        }),
      ],
      activeTabId: "bound-1",
    })
    useTabStore.getState().setDraftDelegationRoute("bound-1", "native")
    const tab = useTabStore.getState().rawTabs.find((t) => t.id === "bound-1")
    expect(tab?.delegationRouteOverride).toBeUndefined()
  })

  it("sameDerivedTab treats override as part of identity (via setDraft recompute)", () => {
    useTabStore.setState({
      rawTabs: [draftTab()],
      activeTabId: "new-1",
    })
    // Force a derived recompute baseline.
    useTabStore.getState().setDraftDelegationRoute("new-1", "codeg")
    const a = useTabStore.getState().tabs.find((t) => t.id === "new-1")
    expect(a?.delegationRouteOverride).toBe("codeg")
    useTabStore.getState().setDraftDelegationRoute("new-1", "native")
    const b = useTabStore.getState().tabs.find((t) => t.id === "new-1")
    expect(b?.delegationRouteOverride).toBe("native")
    // Distinct derived values when override changes.
    expect(a?.delegationRouteOverride).not.toBe(b?.delegationRouteOverride)
  })

  it("bindConversationTab clears memory-only override", () => {
    useTabStore.setState({
      rawTabs: [draftTab({ delegationRouteOverride: "native" })],
      activeTabId: "new-1",
    })
    useTabStore
      .getState()
      .bindConversationTab("new-1", 99, "codex", "First", -1)
    const tab = useTabStore.getState().rawTabs.find((t) => t.id === "new-1")
    expect(tab?.conversationId).toBe(99)
    expect(tab?.delegationRouteOverride).toBeUndefined()
  })
})
