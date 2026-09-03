import { beforeEach, describe, expect, it } from "vitest"
import {
  resetAppWorkspaceStore,
  useAppWorkspaceStore,
} from "@/stores/app-workspace-store"
import {
  resetTabStore,
  useTabStore,
  type TabItemInternal,
} from "@/stores/tab-store"
import type { FolderDetail } from "@/lib/types"

function makeFolder(
  overrides: Partial<FolderDetail> & { id: number }
): FolderDetail {
  return {
    name: `folder-${overrides.id}`,
    path: `/repo/folder-${overrides.id}`,
    git_branch: null,
    default_agent_type: null,
    last_agent_type: null,
    last_opened_at: "2026-01-01T00:00:00.000Z",
    sort_order: overrides.id,
    color: "inherit",
    parent_id: null,
    kind: "regular",
    alias: null,
    group_id: null,
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
    isPinned: false,
    workingDir: "/repo/folder-12",
    ...overrides,
  }
}

beforeEach(() => {
  resetAppWorkspaceStore()
  resetTabStore()
})

describe("disposeDraftBindingForRemovedFolder", () => {
  it("labels removed-folder draft disposal as draft retarget", async () => {
    const acpDisconnect = vi.fn(async () => {})
    useTabStore.getState().setSideEffects({
      activateConversationPane: () => {},
      acpDisconnect,
    })
    useAppWorkspaceStore.setState({
      folders: [makeFolder({ id: 3 })],
      allFolders: [makeFolder({ id: 3 }), makeFolder({ id: 12 })],
    })
    const draft = draftTab({ id: "draft-dispose", folderId: 12 })
    useTabStore.setState({ rawTabs: [draft], tabs: [draft] })

    useTabStore.getState().disposeDraftBindingForRemovedFolder(12)
    await vi.waitFor(() => {
      expect(acpDisconnect).toHaveBeenCalledWith(
        "draft-dispose",
        "draft_retarget"
      )
    })
  })

  it("retargets draft and recomputes both rawTabs and tabs projections", () => {
    const keep = makeFolder({ id: 3 })
    const gone = makeFolder({ id: 12 })
    // Open list after UserRemove already dropped 12.
    useAppWorkspaceStore.setState({
      folders: [keep],
      allFolders: [keep, gone],
    })

    const draft = draftTab({
      id: "draft-12",
      folderId: 12,
      workingDir: gone.path,
    })
    // Intentionally seed a stale `tabs` projection that still points at 12 —
    // raw setState would leave it stale; the action must recompute.
    useTabStore.setState({
      rawTabs: [draft],
      tabs: [{ ...draft, title: "stale-decorated" }],
      activeTabId: "draft-12",
    })

    useTabStore.getState().disposeDraftBindingForRemovedFolder(12)

    const st = useTabStore.getState()
    expect(st.rawTabs).toHaveLength(1)
    expect(st.tabs).toHaveLength(1)
    expect(st.rawTabs[0]?.folderId).toBe(3)
    expect(st.tabs[0]?.folderId).toBe(3)
    expect(st.rawTabs[0]?.workingDir).toBe(keep.path)
    expect(st.tabs[0]?.workingDir).toBe(keep.path)
    // Decorated projection was recomputed from rawTabs (not left at stale title).
    expect(st.tabs[0]?.title).toBe("New")
  })

  it("detaches to chat shell when no other open folder remains", () => {
    useAppWorkspaceStore.setState({
      folders: [],
      allFolders: [makeFolder({ id: 12 })],
    })
    const draft = draftTab({ id: "draft-only", folderId: 12 })
    useTabStore.setState({
      rawTabs: [draft],
      tabs: [draft],
      activeTabId: "draft-only",
    })

    useTabStore.getState().disposeDraftBindingForRemovedFolder(12)

    const st = useTabStore.getState()
    expect(st.rawTabs[0]?.folderId).toBe(-1)
    expect(st.tabs[0]?.folderId).toBe(-1)
    expect(st.rawTabs[0]?.isChat).toBe(true)
    expect(st.tabs[0]?.isChat).toBe(true)
  })
})
