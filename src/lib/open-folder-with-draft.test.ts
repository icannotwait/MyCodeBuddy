import { beforeEach, describe, expect, it, vi } from "vitest"

const openFolderApi = vi.fn()
const openFolderByIdApi = vi.fn()
const openWorktreeFolderApi = vi.fn()

vi.mock("@/lib/api", () => ({
  listOpenedTabs: vi.fn(async () => []),
  saveOpenedTabs: vi.fn(async () => ({
    accepted: true,
    version: 1,
    tabs: [],
  })),
  getFolderConversation: vi.fn(),
  closeFolderIfEmpty: vi.fn(async () => ({ closed: false })),
  listOpenFolderDetails: vi.fn(async () => []),
  listAllFolderDetails: vi.fn(async () => []),
  listAllConversations: vi.fn(async () => []),
  openFolder: (...args: unknown[]) => openFolderApi(...args),
  openFolderById: (...args: unknown[]) => openFolderByIdApi(...args),
  openWorktreeFolder: (...args: unknown[]) => openWorktreeFolderApi(...args),
  removeFolderFromWorkspace: vi.fn(),
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
  openFolderByIdWithDraft,
  openFolderWithDraft,
  openWorktreeFolderWithDraft,
} from "@/lib/open-folder-with-draft"
import {
  resetAppWorkspaceStore,
  useAppWorkspaceStore,
} from "@/stores/app-workspace-store"
import { resetTabStore, useTabStore } from "@/stores/tab-store"
import type { FolderDetail } from "@/lib/types"

function makeFolder(
  overrides: Partial<FolderDetail> & { id: number; path: string }
): FolderDetail {
  const { id, path, ...rest } = overrides
  return {
    id,
    name: overrides.name ?? `folder-${id}`,
    path,
    git_branch: null,
    default_agent_type: overrides.default_agent_type ?? null,
    last_agent_type: overrides.last_agent_type ?? null,
    last_opened_at: "2026-01-01T00:00:00.000Z",
    sort_order: overrides.id,
    color: "inherit",
    parent_id: null,
    kind: "regular",
    alias: null,
    group_id: null,
    ...rest,
  }
}

beforeEach(() => {
  resetAppWorkspaceStore()
  resetTabStore()
  openFolderApi.mockReset()
  openFolderByIdApi.mockReset()
  openWorktreeFolderApi.mockReset()
})

describe("openFolderWithDraft mediator", () => {
  it("user open ensures one draft targeting the folder", async () => {
    const detail = makeFolder({
      id: 42,
      path: "/proj/empty",
      default_agent_type: "codex",
    })
    openFolderApi.mockResolvedValue(detail)

    const result = await openFolderWithDraft("/proj/empty")

    expect(result.id).toBe(42)
    expect(openFolderApi).toHaveBeenCalledWith("/proj/empty")
    const tabs = useTabStore.getState().rawTabs
    expect(tabs).toHaveLength(1)
    expect(tabs[0]?.conversationId).toBeNull()
    expect(tabs[0]?.folderId).toBe(42)
    expect(tabs[0]?.workingDir).toBe("/proj/empty")
    expect(useTabStore.getState().activeTabId).toBe(tabs[0]?.id)
  })

  it("low-level openFolder does not create or focus a draft", async () => {
    const detail = makeFolder({ id: 7, path: "/sys/reg" })
    openFolderApi.mockResolvedValue(detail)

    await useAppWorkspaceStore.getState().openFolder("/sys/reg")

    expect(openFolderApi).toHaveBeenCalledWith("/sys/reg")
    expect(useTabStore.getState().rawTabs).toEqual([])
    expect(useTabStore.getState().activeTabId).toBeNull()
    expect(
      useAppWorkspaceStore.getState().folders.some((f) => f.id === 7)
    ).toBe(true)
  })

  it("openWorktreeFolderWithDraft opens membership + draft", async () => {
    const detail = makeFolder({ id: 9, path: "/wt/feature" })
    openWorktreeFolderApi.mockResolvedValue(detail)

    await openWorktreeFolderWithDraft("/wt/feature", 1)

    expect(openWorktreeFolderApi).toHaveBeenCalledWith("/wt/feature", 1)
    expect(useTabStore.getState().rawTabs[0]?.folderId).toBe(9)
  })

  it("openFolderByIdWithDraft opens membership + draft", async () => {
    const detail = makeFolder({ id: 11, path: "/hist/proj" })
    openFolderByIdApi.mockResolvedValue(detail)

    await openFolderByIdWithDraft(11)

    expect(openFolderByIdApi).toHaveBeenCalledWith(11)
    expect(useTabStore.getState().rawTabs[0]?.folderId).toBe(11)
  })

  it("deep-link style addFolderToWorkspaceById stays silent (no draft)", async () => {
    const detail = makeFolder({ id: 15, path: "/deep/link" })
    openFolderByIdApi.mockResolvedValue(detail)

    await useAppWorkspaceStore.getState().addFolderToWorkspaceById(15)

    expect(useTabStore.getState().rawTabs).toEqual([])
    expect(useTabStore.getState().activeTabId).toBeNull()
  })
})
