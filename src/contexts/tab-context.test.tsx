import { act, render, screen, waitFor } from "@testing-library/react"
import { useEffect } from "react"
import { beforeEach, describe, expect, it, vi } from "vitest"
import { TabProvider, useTabContext } from "@/contexts/tab-context"
import type {
  AgentType,
  DbConversationSummary,
  FolderDetail,
} from "@/lib/types"

const listOpenedTabsMock = vi.fn()
const saveOpenedTabsMock = vi.fn()
const setActiveFolderIdMock = vi.fn()
const activateConversationPaneMock = vi.fn()
const disconnectMock = vi.fn()

vi.mock("next-intl", () => ({
  useTranslations: () => (key: string) => key,
}))

vi.mock("@/lib/api", () => ({
  listOpenedTabs: (...args: unknown[]) => listOpenedTabsMock(...args),
  saveOpenedTabs: (...args: unknown[]) => saveOpenedTabsMock(...args),
}))

vi.mock("@/contexts/app-workspace-context", () => ({
  useAppWorkspace: () => ({
    conversations: conversationsMock,
    folders: foldersMock,
    foldersHydrated: true,
    setActiveFolderId: setActiveFolderIdMock,
  }),
}))

vi.mock("@/contexts/workspace-context", () => ({
  useWorkspaceContext: () => ({
    activateConversationPane: activateConversationPaneMock,
  }),
}))

vi.mock("@/contexts/acp-connections-context", () => ({
  useAcpActions: () => ({
    disconnect: disconnectMock,
  }),
}))

vi.mock("@/hooks/use-sorted-available-agents", () => ({
  useSortedAvailableAgents: () => ({
    sortedTypes: ["codex" satisfies AgentType],
    fresh: true,
  }),
}))

const defaultFoldersMock: FolderDetail[] = [
  {
    id: 1,
    name: "repo",
    path: "/repo",
    git_branch: null,
    default_agent_type: "codex",
    last_opened_at: "2026-05-24T00:00:00Z",
    sort_order: 0,
    color: "blue",
  },
  {
    id: 2,
    name: "other",
    path: "/other",
    git_branch: null,
    default_agent_type: "codex",
    last_opened_at: "2026-05-24T00:00:00Z",
    sort_order: 1,
    color: "green",
  },
]

let foldersMock: FolderDetail[] = defaultFoldersMock

const conversationsMock: DbConversationSummary[] = [
  {
    id: 1,
    folder_id: 1,
    title: "First",
    agent_type: "codex",
    status: "in_progress",
    model: null,
    git_branch: null,
    external_id: null,
    message_count: 1,
    created_at: "2026-05-24T00:00:00Z",
    updated_at: "2026-05-24T00:00:00Z",
  },
  {
    id: 2,
    folder_id: 1,
    title: "Second",
    agent_type: "codex",
    status: "in_progress",
    model: null,
    git_branch: null,
    external_id: null,
    message_count: 1,
    created_at: "2026-05-24T00:00:00Z",
    updated_at: "2026-05-24T00:00:00Z",
  },
  {
    id: 3,
    folder_id: 2,
    title: "Third",
    agent_type: "codex",
    status: "in_progress",
    model: null,
    git_branch: null,
    external_id: null,
    message_count: 1,
    created_at: "2026-05-24T00:00:00Z",
    updated_at: "2026-05-24T00:00:00Z",
  },
]

let latestContext: ReturnType<typeof useTabContext> | null = null

function Probe() {
  const ctx = useTabContext()
  const activeTab = ctx.tabs.find((tab) => tab.id === ctx.activeTabId)

  useEffect(() => {
    latestContext = ctx
  }, [ctx])

  return (
    <div>
      <output data-testid="active">{ctx.activeTabId ?? "none"}</output>
      <output data-testid="tabs">
        {ctx.tabs.map((tab) => tab.id).join(",")}
      </output>
      <output data-testid="active-folder">
        {activeTab?.folderId ?? "none"}
      </output>
    </div>
  )
}

function renderTabs() {
  latestContext = null
  return render(
    <TabProvider>
      <Probe />
    </TabProvider>
  )
}

function openConversationTab(
  folderId: number,
  conversationId: number,
  title: string
) {
  act(() => {
    latestContext?.openTab(folderId, conversationId, "codex", true, title)
  })
}

describe("TabProvider tab state transitions", () => {
  beforeEach(() => {
    vi.clearAllMocks()
    foldersMock = defaultFoldersMock
    listOpenedTabsMock.mockReturnValue(new Promise(() => {}))
  })

  it("activates the neighboring tab when another tab update is already queued", () => {
    renderTabs()

    expect(latestContext).not.toBeNull()

    openConversationTab(1, 1, "First")
    openConversationTab(1, 2, "Second")
    act(() => {
      latestContext?.switchTab("conv-1-codex-1")
    })

    expect(screen.getByTestId("active")).toHaveTextContent("conv-1-codex-1")

    act(() => {
      latestContext?.setTabRuntimeConversationId("conv-1-codex-1", -1)
      latestContext?.closeTab("conv-1-codex-1")
    })

    expect(screen.getByTestId("tabs")).toHaveTextContent("conv-1-codex-2")
    expect(screen.getByTestId("active")).toHaveTextContent("conv-1-codex-2")
  })

  it("keeps the current active tab when closing an inactive tab", () => {
    renderTabs()

    expect(latestContext).not.toBeNull()

    openConversationTab(1, 1, "First")
    openConversationTab(1, 2, "Second")
    act(() => {
      latestContext?.switchTab("conv-1-codex-1")
    })

    expect(screen.getByTestId("active")).toHaveTextContent("conv-1-codex-1")

    act(() => {
      latestContext?.closeTab("conv-1-codex-2")
    })

    expect(screen.getByTestId("tabs")).toHaveTextContent("conv-1-codex-1")
    expect(screen.getByTestId("active")).toHaveTextContent("conv-1-codex-1")
  })

  it("creates and activates a replacement draft when closing the last tab with folders available", () => {
    renderTabs()

    expect(latestContext).not.toBeNull()

    openConversationTab(1, 1, "First")

    act(() => {
      latestContext?.closeTab("conv-1-codex-1")
    })

    const tabsText = screen.getByTestId("tabs").textContent ?? ""
    expect(tabsText).toMatch(/^new-/)
    expect(screen.getByTestId("active")).toHaveTextContent(tabsText)
  })

  it("clears the active tab when closing the last tab with no folders available", () => {
    foldersMock = []
    renderTabs()

    expect(latestContext).not.toBeNull()

    openConversationTab(1, 1, "First")

    act(() => {
      latestContext?.closeTab("conv-1-codex-1")
    })

    expect(screen.getByTestId("tabs")).toHaveTextContent("")
    expect(screen.getByTestId("active")).toHaveTextContent("none")
  })

  it("activates a remaining tab when closing a folder after switching to one of its tabs in the same batch", () => {
    renderTabs()

    expect(latestContext).not.toBeNull()

    openConversationTab(1, 1, "First")
    openConversationTab(1, 2, "Second")
    openConversationTab(2, 3, "Third")
    act(() => {
      latestContext?.switchTab("conv-2-codex-3")
    })

    expect(screen.getByTestId("active")).toHaveTextContent("conv-2-codex-3")

    act(() => {
      latestContext?.switchTab("conv-1-codex-1")
      latestContext?.closeTabsByFolder(1)
    })

    expect(screen.getByTestId("tabs")).toHaveTextContent("conv-2-codex-3")
    expect(screen.getByTestId("active")).toHaveTextContent("conv-2-codex-3")
  })

  it("ignores closeOtherTabs when its target was removed earlier in the same batch", () => {
    renderTabs()

    expect(latestContext).not.toBeNull()

    openConversationTab(1, 1, "First")
    openConversationTab(1, 2, "Second")

    act(() => {
      latestContext?.closeTab("conv-1-codex-1")
      latestContext?.closeOtherTabs("conv-1-codex-1")
    })

    expect(screen.getByTestId("tabs")).toHaveTextContent("conv-1-codex-2")
    expect(screen.getByTestId("active")).toHaveTextContent("conv-1-codex-2")
  })

  it("keeps an existing draft active when reopening a draft after closing it in the same batch", () => {
    renderTabs()

    expect(latestContext).not.toBeNull()

    act(() => {
      latestContext?.openNewConversationTab(1, "/repo")
    })

    const draftTabId = latestContext?.activeTabId
    expect(draftTabId).toMatch(/^new-/)

    act(() => {
      latestContext?.closeTab(draftTabId!)
      latestContext?.openNewConversationTab(1, "/repo")
    })

    const tabsText = screen.getByTestId("tabs").textContent ?? ""
    expect(tabsText).toMatch(/^new-/)
    expect(screen.getByTestId("active")).toHaveTextContent(tabsText)
  })

  it("retargets the replacement draft when reopening a closed draft for another folder in the same batch", async () => {
    renderTabs()

    expect(latestContext).not.toBeNull()

    act(() => {
      latestContext?.openNewConversationTab(1, "/repo")
    })

    const draftTabId = latestContext?.activeTabId
    expect(draftTabId).toMatch(/^new-/)

    act(() => {
      latestContext?.closeTab(draftTabId!)
      latestContext?.openNewConversationTab(2, "/other")
    })

    const replacementTabId = screen.getByTestId("tabs").textContent ?? ""
    expect(replacementTabId).toMatch(/^new-/)
    expect(replacementTabId).not.toBe(draftTabId)
    expect(screen.getByTestId("active")).toHaveTextContent(replacementTabId)

    await waitFor(() => {
      expect(disconnectMock).toHaveBeenCalledWith(replacementTabId)
      expect(screen.getByTestId("active-folder")).toHaveTextContent("2")
    })
  })

  it("activates an opened tab when another tab update is already queued", () => {
    renderTabs()

    expect(latestContext).not.toBeNull()

    openConversationTab(1, 1, "First")

    expect(screen.getByTestId("active")).toHaveTextContent("conv-1-codex-1")

    act(() => {
      latestContext?.setTabRuntimeConversationId("conv-1-codex-1", -1)
      latestContext?.openTab(1, 2, "codex", true, "Second")
    })

    expect(screen.getByTestId("tabs")).toHaveTextContent("conv-1-codex-1")
    expect(screen.getByTestId("tabs")).toHaveTextContent("conv-1-codex-2")
    expect(screen.getByTestId("active")).toHaveTextContent("conv-1-codex-2")
  })

  it("keeps the retained draft tab active when binding it over an existing duplicate conversation tab", () => {
    renderTabs()

    expect(latestContext).not.toBeNull()

    openConversationTab(1, 1, "First")
    act(() => {
      latestContext?.openNewConversationTab(1, "/repo")
    })

    const draftTabId = latestContext?.activeTabId
    expect(draftTabId).toMatch(/^new-/)

    act(() => {
      latestContext?.setTabRuntimeConversationId(draftTabId!, -1)
      latestContext?.bindConversationTab(draftTabId!, 1, "codex", "First", -1)
    })

    expect(screen.getByTestId("tabs")).toHaveTextContent(draftTabId!)
    expect(screen.getByTestId("tabs").textContent).not.toContain(
      "conv-1-codex-1"
    )
    expect(screen.getByTestId("active")).toHaveTextContent(draftTabId!)
  })

  it("does not report a preview replacement for a preview tab already closed in the same batch", () => {
    const replacedTabIds: string[] = []
    renderTabs()

    expect(latestContext).not.toBeNull()

    latestContext?.onPreviewTabReplaced((tabId) => {
      replacedTabIds.push(tabId)
    })
    act(() => {
      latestContext?.openTab(1, 1, "codex", false, "First")
    })

    expect(screen.getByTestId("active")).toHaveTextContent("conv-1-codex-1")

    act(() => {
      latestContext?.closeTab("conv-1-codex-1")
      latestContext?.openTab(1, 2, "codex", false, "Second")
    })

    expect(screen.getByTestId("tabs")).toHaveTextContent("conv-1-codex-2")
    expect(screen.getByTestId("active")).toHaveTextContent("conv-1-codex-2")
    expect(replacedTabIds).toEqual([])
  })
})
