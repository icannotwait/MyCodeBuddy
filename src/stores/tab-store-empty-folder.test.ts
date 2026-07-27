import { beforeEach, describe, expect, it, vi } from "vitest"

const closeFolderIfEmpty = vi.fn()
const listOpenFolderDetails = vi.fn(async () => [] as unknown[])
const listAllFolderDetails = vi.fn(async () => [] as unknown[])

vi.mock("@/lib/api", () => ({
  listOpenedTabs: vi.fn(async () => []),
  saveOpenedTabs: vi.fn(async () => ({
    accepted: true,
    version: 1,
    tabs: [],
  })),
  getFolderConversation: vi.fn(),
  closeFolderIfEmpty: (...args: unknown[]) => closeFolderIfEmpty(...args),
  listOpenFolderDetails: (...args: unknown[]) => listOpenFolderDetails(...args),
  listAllFolderDetails: (...args: unknown[]) => listAllFolderDetails(...args),
  listAllConversations: vi.fn(async () => []),
  openFolder: vi.fn(),
  openFolderById: vi.fn(),
  openWorktreeFolder: vi.fn(),
  removeFolderFromWorkspace: vi.fn(),
  reorderFolders: vi.fn(),
  getFolder: vi.fn(),
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
  resetAppWorkspaceStore,
  useAppWorkspaceStore,
} from "@/stores/app-workspace-store"
import {
  resetTabStore,
  useTabStore,
  type TabItemInternal,
} from "@/stores/tab-store"
import type { DbConversationSummary, FolderDetail } from "@/lib/types"

function makeFolder(
  overrides: Partial<FolderDetail> & { id: number; path?: string }
): FolderDetail {
  return {
    name: `folder-${overrides.id}`,
    path: overrides.path ?? `/repo/folder-${overrides.id}`,
    git_branch: null,
    default_agent_type: null,
    last_agent_type: null,
    last_opened_at: "2026-01-01T00:00:00.000Z",
    sort_order: overrides.id,
    color: "inherit",
    parent_id: null,
    kind: "regular",
    alias: null,
    ...overrides,
  }
}

function draftTab(overrides: Partial<TabItemInternal> = {}): TabItemInternal {
  return {
    id: "draft-1",
    kind: "conversation",
    conversationId: null,
    folderId: 12,
    agentType: "claude_code",
    title: "New",
    isPinned: true,
    workingDir: "/repo/folder-12",
    ...overrides,
  }
}

function liveConv(folderId: number, id = 100): DbConversationSummary {
  return {
    id,
    folder_id: folderId,
    title: "Live",
    title_locked: false,
    auto_title_finalized: false,
    agent_type: "claude_code",
    status: "completed",
    awaiting_reply_token: null,
    kind: "regular",
    model: null,
    git_branch: null,
    external_id: null,
    message_count: 1,
    child_count: 0,
    created_at: "2026-01-01T00:00:00.000Z",
    updated_at: "2026-01-01T00:00:00.000Z",
    pinned_at: null,
  }
}

beforeEach(() => {
  resetAppWorkspaceStore()
  resetTabStore()
  closeFolderIfEmpty.mockReset()
  listOpenFolderDetails.mockReset()
  listAllFolderDetails.mockReset()
  listOpenFolderDetails.mockResolvedValue([])
  listAllFolderDetails.mockResolvedValue([])
  closeFolderIfEmpty.mockResolvedValue({ closed: true })
})

describe("draft leave → conditional close", () => {
  it("close draft on empty folder calls closeFolderIfEmpty (not user-remove)", async () => {
    const f = makeFolder({ id: 12 })
    useAppWorkspaceStore.setState({
      folders: [f, makeFolder({ id: 3 })],
      allFolders: [f, makeFolder({ id: 3 })],
      conversations: [],
    })
    useTabStore.setState({
      rawTabs: [
        draftTab({ id: "draft-12", folderId: 12 }),
        {
          id: "conv-3",
          kind: "conversation",
          folderId: 3,
          conversationId: 1,
          agentType: "claude_code",
          title: "Other",
          isPinned: false,
        },
      ],
      activeTabId: "draft-12",
      tabsHydrated: true,
    })

    useTabStore.getState().closeTab("draft-12")

    await vi.waitFor(() => {
      expect(closeFolderIfEmpty).toHaveBeenCalledWith(12)
    })
    expect(closeFolderIfEmpty).toHaveBeenCalledTimes(1)
  })

  it("close draft with live conversations does not call closeFolderIfEmpty", async () => {
    const f = makeFolder({ id: 12 })
    useAppWorkspaceStore.setState({
      folders: [f],
      allFolders: [f],
      conversations: [liveConv(12)],
    })
    useTabStore.setState({
      rawTabs: [draftTab({ id: "draft-12" })],
      activeTabId: "draft-12",
      tabsHydrated: true,
    })

    useTabStore.getState().closeTab("draft-12")

    // Replacement draft may be created; leave predicate must skip API.
    await Promise.resolve()
    await Promise.resolve()
    expect(closeFolderIfEmpty).not.toHaveBeenCalled()
  })

  it("last-tab close on sole empty folder drops F and does not rebind replacement to F", async () => {
    const f = makeFolder({ id: 12 })
    const g = makeFolder({ id: 3, path: "/repo/g" })
    useAppWorkspaceStore.setState({
      folders: [f, g],
      allFolders: [f, g],
      conversations: [],
    })
    useTabStore.setState({
      rawTabs: [
        draftTab({ id: "draft-only", folderId: 12, workingDir: f.path }),
      ],
      activeTabId: "draft-only",
      tabsHydrated: true,
    })

    let resolveClose!: (v: { closed: boolean }) => void
    closeFolderIfEmpty.mockImplementation(
      () =>
        new Promise<{ closed: boolean }>((resolve) => {
          resolveClose = resolve
        })
    )

    useTabStore.getState().closeTab("draft-only")

    // While API in flight: F already dropped; replacement must not bind F.
    const st = useTabStore.getState()
    expect(useAppWorkspaceStore.getState().folders.map((x) => x.id)).toEqual([
      3,
    ])
    expect(st.rawTabs).toHaveLength(1)
    expect(st.rawTabs[0]?.folderId).toBe(3)
    expect(st.rawTabs[0]?.folderId).not.toBe(12)

    resolveClose({ closed: true })
    await vi.waitFor(() => {
      expect(closeFolderIfEmpty).toHaveBeenCalledWith(12)
    })
  })

  it("last-tab close sole empty folder with no other folders yields empty workspace", async () => {
    const f = makeFolder({ id: 12 })
    useAppWorkspaceStore.setState({
      folders: [f],
      allFolders: [f],
      conversations: [],
    })
    useTabStore.setState({
      rawTabs: [draftTab({ id: "draft-only", folderId: 12 })],
      activeTabId: "draft-only",
      tabsHydrated: true,
    })

    useTabStore.getState().closeTab("draft-only")

    expect(useAppWorkspaceStore.getState().folders).toEqual([])
    expect(useTabStore.getState().rawTabs).toEqual([])
    expect(useTabStore.getState().activeTabId).toBeNull()
    await vi.waitFor(() => {
      expect(closeFolderIfEmpty).toHaveBeenCalledWith(12)
    })
  })

  it("retarget draft A→B with A empty calls closeFolderIfEmpty for A", async () => {
    const a = makeFolder({ id: 1, path: "/a" })
    const b = makeFolder({ id: 2, path: "/b" })
    useAppWorkspaceStore.setState({
      folders: [a, b],
      allFolders: [a, b],
      conversations: [],
    })
    useTabStore.setState({
      rawTabs: [
        draftTab({
          id: "draft",
          folderId: 1,
          workingDir: "/a",
          agentType: "claude_code",
          agentTypeProvisional: false,
        }),
      ],
      activeTabId: "draft",
      tabsHydrated: true,
      draftRetargetRequests: [],
    })
    useTabStore.getState().setSideEffects({
      activateConversationPane: () => {},
      acpDisconnect: async () => {},
    })

    useTabStore.getState().openNewConversationTab(2, "/b")
    // Consume retarget queue (TabRuntimeEffects normally does this).
    useTabStore.getState().consumeDraftRetargets()

    await vi.waitFor(() => {
      expect(useTabStore.getState().rawTabs[0]?.folderId).toBe(2)
    })
    await vi.waitFor(() => {
      expect(closeFolderIfEmpty).toHaveBeenCalledWith(1)
    })
  })

  it("closed:true still drops when event is suppressed", async () => {
    const f = makeFolder({ id: 12 })
    const g = makeFolder({ id: 3 })
    useAppWorkspaceStore.setState({
      folders: [f, g],
      allFolders: [f, g],
      conversations: [],
    })
    useTabStore.setState({
      rawTabs: [
        draftTab({ id: "d", folderId: 12 }),
        {
          id: "c3",
          kind: "conversation",
          folderId: 3,
          conversationId: 9,
          agentType: "claude_code",
          title: "x",
          isPinned: false,
        },
      ],
      activeTabId: "d",
      tabsHydrated: true,
    })
    // Non-last close: F remains until API result.
    closeFolderIfEmpty.mockResolvedValue({ closed: true })
    listOpenFolderDetails.mockResolvedValue([g])
    listAllFolderDetails.mockResolvedValue([f, g])

    useTabStore.getState().closeTab("d")
    await vi.waitFor(() => {
      expect(closeFolderIfEmpty).toHaveBeenCalledWith(12)
    })
    await vi.waitFor(() => {
      expect(
        useAppWorkspaceStore.getState().folders.some((x) => x.id === 12)
      ).toBe(false)
    })
  })

  it("closed:false triggers fenced refetch and does not recreate draft on F", async () => {
    const f = makeFolder({ id: 12 })
    const g = makeFolder({ id: 3 })
    useAppWorkspaceStore.setState({
      folders: [f, g],
      allFolders: [f, g],
      conversations: [],
    })
    useTabStore.setState({
      rawTabs: [
        draftTab({ id: "d", folderId: 12 }),
        {
          id: "c3",
          kind: "conversation",
          folderId: 3,
          conversationId: 9,
          agentType: "claude_code",
          title: "x",
          isPinned: false,
        },
      ],
      activeTabId: "d",
      tabsHydrated: true,
    })
    closeFolderIfEmpty.mockResolvedValue({ closed: false })
    // Server still lists F (e.g. concurrent live appeared).
    listOpenFolderDetails.mockResolvedValue([f, g])
    listAllFolderDetails.mockResolvedValue([f, g])

    useTabStore.getState().closeTab("d")
    await vi.waitFor(() => {
      expect(closeFolderIfEmpty).toHaveBeenCalledWith(12)
    })
    await vi.waitFor(() => {
      expect(listOpenFolderDetails).toHaveBeenCalled()
    })
    // Must not invent a new draft solely because close was false.
    expect(
      useTabStore
        .getState()
        .rawTabs.some((t) => t.conversationId == null && t.folderId === 12)
    ).toBe(false)
  })

  it("transport error refetches and retries once when still open + leave holds", async () => {
    const f = makeFolder({ id: 12 })
    const g = makeFolder({ id: 3 })
    useAppWorkspaceStore.setState({
      folders: [f, g],
      allFolders: [f, g],
      conversations: [],
    })
    useTabStore.setState({
      rawTabs: [
        draftTab({ id: "d", folderId: 12 }),
        {
          id: "c3",
          kind: "conversation",
          folderId: 3,
          conversationId: 9,
          agentType: "claude_code",
          title: "x",
          isPinned: false,
        },
      ],
      activeTabId: "d",
      tabsHydrated: true,
    })
    closeFolderIfEmpty
      .mockRejectedValueOnce(new Error("network"))
      .mockResolvedValueOnce({ closed: true })
    listOpenFolderDetails.mockResolvedValue([f, g])
    listAllFolderDetails.mockResolvedValue([f, g])

    useTabStore.getState().closeTab("d")
    await vi.waitFor(() => {
      expect(closeFolderIfEmpty).toHaveBeenCalledTimes(2)
    })
  })
})
