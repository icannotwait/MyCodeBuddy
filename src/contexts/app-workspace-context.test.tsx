import { act, render, screen, waitFor } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"
import { AppWorkspaceProvider } from "@/contexts/app-workspace-context"
import {
  resetAppWorkspaceStore,
  useAppWorkspaceStore,
} from "@/stores/app-workspace-store"
import { resetTabStore, useTabStore } from "@/stores/tab-store"
import { getActiveBackendCacheKey } from "@/lib/transport"
import type {
  ConversationChange,
  DbConversationSummary,
  FolderChange,
  FolderDetail,
} from "@/lib/types"

type OpenFolderById = typeof import("@/lib/api").openFolderById
type GetGitHead = typeof import("@/lib/api").getGitHead

// Capture the `conversation://changed` handler + reconnect callback the
// provider registers, plus dispose/unsub spies, so tests can drive events and
// assert cleanup. `vi.hoisted` runs before the (hoisted) mock factories so they
// can close over this shared state without a TDZ error.
const h = vi.hoisted(() => ({
  handler: null as null | ((change: unknown) => void),
  folderHandler: null as null | ((change: unknown) => void),
  bulkHandler: null as null | ((change: unknown) => void),
  groupHandler: null as null | ((change: unknown) => void),
  reconnect: null as null | (() => void),
  folderReconnect: null as null | (() => void),
  disposeSpy: vi.fn(),
  folderDisposeSpy: vi.fn(),
  bulkDisposeSpy: vi.fn(),
  groupDisposeSpy: vi.fn(),
  reconnectUnsubSpy: vi.fn(),
  folderReconnectUnsubSpy: vi.fn(),
  listAll: vi.fn(async () => [] as unknown[]),
  listOpenFolders: vi.fn(async () => [] as unknown[]),
  listFolderGroups: vi.fn(async () => [] as unknown[]),
  listAllFolders: vi.fn(async () => [] as unknown[]),
  openFolderById: vi.fn<OpenFolderById>(async (folderId) => ({
    id: folderId,
    name: `folder-${folderId}`,
    path: `/repo/folder-${folderId}`,
    git_branch: null,
    default_agent_type: null,
    last_agent_type: null,
    last_opened_at: "2026-01-01T00:00:00.000Z",
    sort_order: folderId,
    color: "inherit",
    parent_id: null,
    kind: "regular",
    alias: null,
    group_id: null,
  })),
  getGitHead: vi.fn<GetGitHead>(async () => ({
    is_repo: false,
    branch: null,
    detached: false,
    short_sha: null,
  })),
  conversationExperienceBootstrap: vi.fn(),
  delegationProfileBootstrap: vi.fn(),
  useAcpAgents: vi.fn(() => ({ agents: [], fresh: false, refresh: vi.fn() })),
  markConversationUpsert: vi.fn(),
  markConversationStatus: vi.fn(),
  markConversationDelete: vi.fn(),
  applyConversationChange: vi.fn(),
  refetchTracked: vi.fn(),
}))

vi.mock("@/lib/reference-search-cache", () => ({
  referenceSearchCache: {
    markConversationUpsert: h.markConversationUpsert,
    markConversationStatus: h.markConversationStatus,
    markConversationDelete: h.markConversationDelete,
  },
}))

vi.mock("@/lib/delegation-child-projection-cache", () => ({
  delegationChildProjectionCache: {
    applyConversationChange: h.applyConversationChange,
    refetchTracked: h.refetchTracked,
  },
}))

vi.mock("@/lib/platform", () => ({
  // The provider registers four subscriptions — `conversation://changed`,
  // `conversations://bulk-changed`, `folder://changed` and
  // `folder-group://changed` — so route by exact channel and capture each
  // handler / dispose spy independently; the conversation-sync tests keep
  // asserting against `h.handler`/`h.disposeSpy` unchanged. Routing every
  // unmatched channel to `h.handler` would silently hand the newest
  // subscription the conversation tests' handler slot.
  subscribe: vi.fn(async (event: string, handler: (c: unknown) => void) => {
    if (event === "folder://changed") {
      h.folderHandler = handler
      return h.folderDisposeSpy
    }
    if (event === "folder-group://changed") {
      h.groupHandler = handler
      return h.groupDisposeSpy
    }
    if (event === "conversations://bulk-changed") {
      h.bulkHandler = handler
      return h.bulkDisposeSpy
    }
    h.handler = handler
    return h.disposeSpy
  }),
  // Both subscription effects register a reconnect backstop. The folder effect
  // runs after the conversation effect (later in the component body), so the
  // second registration is the folder one; distinct unsub spies keep each
  // subscription's cleanup independently assertable.
  onTransportReconnect: vi.fn((cb: () => void) => {
    if (h.reconnect == null) {
      h.reconnect = cb
      return h.reconnectUnsubSpy
    }
    h.folderReconnect = cb
    return h.folderReconnectUnsubSpy
  }),
}))

vi.mock("@/lib/api", () => ({
  listAllConversations: h.listAll,
  listAllFolderDetails: h.listAllFolders,
  listOpenFolderDetails: h.listOpenFolders,
  listFolderGroups: h.listFolderGroups,
  getGitBranch: vi.fn(async () => null),
  getGitHead: h.getGitHead,
  openFolder: vi.fn(),
  openFolderById: h.openFolderById,
  removeFolderFromWorkspace: vi.fn(),
  applySidebarLayout: vi.fn(),
  createFolderGroup: vi.fn(),
  updateFolderGroup: vi.fn(),
  deleteFolderGroup: vi.fn(),
  setFolderGroup: vi.fn(),
  getFolder: vi.fn(),
}))

// Prevent the conversation-experience settings-event subscription from
// overwriting this suite's intentionally narrow conversation/folder handler capture.
vi.mock("@/stores/conversation-experience-store", () => ({
  useConversationExperienceBootstrap: h.conversationExperienceBootstrap,
}))

// Same for the profile-catalog bootstrap: keep one handler per channel.
vi.mock("@/stores/delegation-profile-store", () => ({
  useDelegationProfileBootstrap: h.delegationProfileBootstrap,
  useDelegationProfileStore: {
    getState: () => ({ ready: false }),
  },
}))

vi.mock("@/hooks/use-acp-agents", () => ({
  useAcpAgents: h.useAcpAgents,
  selectAcpAgentsFresh: () => false,
}))

function makeSummary(
  overrides: Partial<DbConversationSummary> & { id: number }
): DbConversationSummary {
  return {
    folder_id: 1,
    title: null,
    title_locked: false,
    auto_title_finalized: false,
    agent_type: "claude_code",
    status: "in_progress",
    awaiting_reply_token: null,
    kind: "regular",
    model: null,
    git_branch: null,
    external_id: null,
    message_count: 0,
    child_count: 0,
    created_at: "2026-01-01T00:00:00.000Z",
    updated_at: "2026-01-01T00:00:00.000Z",
    pinned_at: null,
    parent_id: null,
    parent_tool_use_id: null,
    delegation_call_id: null,
    ...overrides,
  }
}

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

// Subscribes via selectors the way real consumers do, so the assertions also
// exercise the store's slice reactivity (a missed notify would fail here).
function Probe() {
  const conversations = useAppWorkspaceStore((s) => s.conversations)
  const stats = useAppWorkspaceStore((s) => s.stats)
  const folders = useAppWorkspaceStore((s) => s.folders)
  const allFolders = useAppWorkspaceStore((s) => s.allFolders)
  return (
    <div>
      <output data-testid="ids">
        {conversations.map((c) => c.id).join(",")}
      </output>
      <output data-testid="count">{conversations.length}</output>
      <output data-testid="statuses">
        {conversations.map((c) => `${c.id}:${c.status}`).join(",")}
      </output>
      <output data-testid="stat-total">
        {stats?.total_conversations ?? 0}
      </output>
      <output data-testid="stat-messages">{stats?.total_messages ?? 0}</output>
      <output data-testid="folder-ids">
        {folders.map((f) => f.id).join(",")}
      </output>
      <output data-testid="all-folder-ids">
        {allFolders.map((f) => f.id).join(",")}
      </output>
    </div>
  )
}

async function mountProvider() {
  const utils = render(
    <AppWorkspaceProvider>
      <Probe />
    </AppWorkspaceProvider>
  )
  // Flush mount effects: fetchFolders/refreshConversations + the async
  // subscribe() IIFE that captures the handler.
  await act(async () => {})
  return utils
}

function emit(change: ConversationChange) {
  act(() => {
    h.handler?.(change)
  })
}

function emitFolder(change: FolderChange) {
  act(() => {
    h.folderHandler?.(change)
  })
}

beforeEach(() => {
  h.handler = null
  h.folderHandler = null
  h.bulkHandler = null
  h.reconnect = null
  h.folderReconnect = null
  h.disposeSpy.mockClear()
  h.folderDisposeSpy.mockClear()
  h.bulkDisposeSpy.mockClear()
  h.groupDisposeSpy.mockClear()
  h.reconnectUnsubSpy.mockClear()
  h.folderReconnectUnsubSpy.mockClear()
  h.listAll.mockClear()
  h.listAll.mockResolvedValue([])
  h.listOpenFolders.mockClear()
  h.listOpenFolders.mockResolvedValue([])
  h.listAllFolders.mockClear()
  h.listAllFolders.mockResolvedValue([])
  h.openFolderById.mockClear()
  h.openFolderById.mockImplementation(async (folderId: number) =>
    makeFolder({ id: folderId })
  )
  h.getGitHead.mockReset()
  h.getGitHead.mockResolvedValue({
    is_repo: false,
    branch: null,
    detached: false,
    short_sha: null,
  })
  h.conversationExperienceBootstrap.mockClear()
  h.delegationProfileBootstrap.mockClear()
  h.useAcpAgents.mockClear()
  h.markConversationUpsert.mockClear()
  h.markConversationStatus.mockClear()
  h.markConversationDelete.mockClear()
  h.applyConversationChange.mockClear()
  h.refetchTracked.mockClear()
  // The store is a module-level singleton: restore pristine state (including
  // the delete tombstones) so state can't leak between tests.
  resetAppWorkspaceStore()
  resetTabStore()
})

describe("AppWorkspaceProvider active-folder Git HEAD sync", () => {
  it("loads and applies the active folder head on mount", async () => {
    const folder = makeFolder({ id: 17, path: "/repo/active" })
    const head = {
      is_repo: true,
      branch: "feature/popout",
      detached: false,
      short_sha: null,
      canonical_repo: "/repo/active",
      head_sha: "0123456789abcdef",
      reference_source_epoch: "v1:test",
    }
    h.listOpenFolders.mockResolvedValue([folder])
    h.listAllFolders.mockResolvedValue([folder])
    h.getGitHead.mockResolvedValue(head)
    useAppWorkspaceStore.setState({
      allFolders: [folder],
      activeFolderId: folder.id,
    })

    await mountProvider()

    await waitFor(() => {
      expect(h.getGitHead).toHaveBeenCalledWith(folder.path)
      expect(useAppWorkspaceStore.getState().getBranch(folder.id)).toBe(
        "feature/popout"
      )
    })
    expect(useAppWorkspaceStore.getState().gitHeads.get(folder.id)).toEqual(
      head
    )
  })
})

describe("AppWorkspaceProvider conversation://changed sync", () => {
  it("registers a subscription and reconnect backstop on mount", async () => {
    await mountProvider()
    expect(h.handler).toBeTypeOf("function")
    expect(h.reconnect).toBeTypeOf("function")
  })

  it("forwards_conversation_changes_to_the_reference_cache_once", async () => {
    await mountProvider()
    // Seed an existing row so the status path mutates workspace state.
    const existing = makeSummary({ id: 10, status: "in_progress" })
    emit({ kind: "upsert", summary: existing })
    h.markConversationUpsert.mockClear()

    const upsert = makeSummary({
      id: 10,
      title: "Renamed",
      status: "in_progress",
    })
    emit({ kind: "upsert", summary: upsert })
    emit({
      kind: "state",
      patch: {
        id: 10,
        status: "pending_review",
        awaiting_reply_token: "tok",
        updated_at: "2026-07-16T02:00:00.000Z",
      },
    })
    emit({ kind: "deleted", id: 10 })

    const backend = getActiveBackendCacheKey()
    expect(h.markConversationUpsert).toHaveBeenCalledTimes(1)
    expect(h.markConversationUpsert).toHaveBeenCalledWith(backend, upsert)
    expect(h.markConversationStatus).toHaveBeenCalledTimes(1)
    expect(h.markConversationStatus).toHaveBeenCalledWith(
      backend,
      10,
      "pending_review"
    )
    expect(h.markConversationDelete).toHaveBeenCalledTimes(1)
    expect(h.markConversationDelete).toHaveBeenCalledWith(backend, 10)

    // Original workspace-store behavior still occurs.
    expect(screen.getByTestId("count")).toHaveTextContent("0")
  })

  it("mounts conversation experience bootstrap on provider mount", async () => {
    await mountProvider()
    expect(h.conversationExperienceBootstrap).toHaveBeenCalled()
  })

  it("mounts acp agents and delegation profile bootstrap on provider mount", async () => {
    await mountProvider()
    expect(h.useAcpAgents).toHaveBeenCalled()
    expect(h.delegationProfileBootstrap).toHaveBeenCalled()
  })

  it("inserts a new root conversation, prepending most-recent-first", async () => {
    await mountProvider()
    emit({ kind: "upsert", summary: makeSummary({ id: 1 }) })
    emit({ kind: "upsert", summary: makeSummary({ id: 2 }) })
    expect(screen.getByTestId("ids")).toHaveTextContent("2,1")
    expect(screen.getByTestId("count")).toHaveTextContent("2")
    expect(screen.getByTestId("stat-total")).toHaveTextContent("2")
  })

  it("replaces an existing conversation in place (no reorder) and updates fields", async () => {
    await mountProvider()
    emit({ kind: "upsert", summary: makeSummary({ id: 1 }) })
    emit({ kind: "upsert", summary: makeSummary({ id: 2 }) })
    // Re-upsert id 1 with a new status; it must keep its index (1), not jump.
    emit({
      kind: "upsert",
      summary: makeSummary({ id: 1, status: "pending_review" }),
    })
    expect(screen.getByTestId("ids")).toHaveTextContent("2,1")
    expect(screen.getByTestId("statuses")).toHaveTextContent(
      "2:in_progress,1:pending_review"
    )
  })

  it("ignores delegation children (parent_id set) — not sidebar rows", async () => {
    await mountProvider()
    emit({ kind: "upsert", summary: makeSummary({ id: 1 }) })
    emit({ kind: "upsert", summary: makeSummary({ id: 5, parent_id: 1 }) })
    expect(screen.getByTestId("ids")).toHaveTextContent("1")
    expect(screen.getByTestId("count")).toHaveTextContent("1")
  })

  it("removes on deleted and is idempotent for an unknown id", async () => {
    await mountProvider()
    emit({ kind: "upsert", summary: makeSummary({ id: 1 }) })
    emit({ kind: "upsert", summary: makeSummary({ id: 2 }) })
    emit({ kind: "deleted", id: 1 })
    expect(screen.getByTestId("ids")).toHaveTextContent("2")
    emit({ kind: "deleted", id: 999 })
    expect(screen.getByTestId("ids")).toHaveTextContent("2")
    expect(screen.getByTestId("count")).toHaveTextContent("1")
  })

  it("does not resurrect a row when a stale upsert lands after a delete", async () => {
    await mountProvider()
    emit({ kind: "upsert", summary: makeSummary({ id: 1 }) })
    emit({ kind: "deleted", id: 1 })
    expect(screen.getByTestId("count")).toHaveTextContent("0")
    // A stale/out-of-order upsert for the just-deleted id must be ignored —
    // ids are never reused, so the tombstone is authoritative.
    emit({
      kind: "upsert",
      summary: makeSummary({ id: 1, status: "pending_review" }),
    })
    expect(screen.getByTestId("count")).toHaveTextContent("0")
    expect(screen.getByTestId("ids").textContent).toBe("")
  })

  it("applies a state patch for a known conversation and no-ops for an unknown one", async () => {
    await mountProvider()
    emit({
      kind: "upsert",
      summary: makeSummary({
        id: 1,
        status: "in_progress",
        awaiting_reply_token: null,
        updated_at: "2026-07-16T01:00:00.000Z",
      }),
    })
    const statsBefore = useAppWorkspaceStore.getState().stats
    emit({
      kind: "state",
      patch: {
        id: 1,
        status: "pending_review",
        awaiting_reply_token: "generation-b",
        updated_at: "2026-07-16T02:03:04.000Z",
      },
    })
    expect(screen.getByTestId("statuses")).toHaveTextContent("1:pending_review")
    const row = useAppWorkspaceStore.getState().conversations[0]
    expect(row).toMatchObject({
      status: "pending_review",
      awaiting_reply_token: "generation-b",
      updated_at: "2026-07-16T02:03:04.000Z",
    })
    expect(useAppWorkspaceStore.getState().stats).toBe(statsBefore)
    const conversationsBeforeUnknown =
      useAppWorkspaceStore.getState().conversations
    emit({
      kind: "state",
      patch: {
        id: 999,
        status: "cancelled",
        awaiting_reply_token: null,
        updated_at: "2026-07-16T03:00:00.000Z",
      },
    })
    expect(screen.getByTestId("count")).toHaveTextContent("1")
    expect(screen.getByTestId("statuses")).toHaveTextContent("1:pending_review")
    expect(useAppWorkspaceStore.getState().conversations).toBe(
      conversationsBeforeUnknown
    )
  })

  it("derives stats.total_messages from upserted message counts", async () => {
    await mountProvider()
    emit({ kind: "upsert", summary: makeSummary({ id: 1, message_count: 3 }) })
    emit({ kind: "upsert", summary: makeSummary({ id: 2, message_count: 4 }) })
    expect(screen.getByTestId("stat-total")).toHaveTextContent("2")
    expect(screen.getByTestId("stat-messages")).toHaveTextContent("7")
  })

  it("re-fetches the full list on transport reconnect (disconnect backstop)", async () => {
    await mountProvider()
    expect(h.listAll).toHaveBeenCalledTimes(1) // initial mount fetch
    await act(async () => {
      h.reconnect?.()
    })
    expect(h.listAll).toHaveBeenCalledTimes(2)
  })

  it("forwards conversation changes to the child projection cache", async () => {
    await mountProvider()
    const upsert = makeSummary({ id: 42, title: "Child" })
    emit({ kind: "upsert", summary: upsert })
    emit({ kind: "deleted", id: 42 })
    emit({
      kind: "state",
      patch: {
        id: 42,
        status: "pending_review",
        awaiting_reply_token: null,
        updated_at: "2026-07-19T00:00:00.000Z",
      },
    })

    expect(h.applyConversationChange).toHaveBeenCalledTimes(3)
    expect(h.applyConversationChange).toHaveBeenNthCalledWith(1, {
      kind: "upsert",
      summary: upsert,
    })
    expect(h.applyConversationChange).toHaveBeenNthCalledWith(2, {
      kind: "deleted",
      id: 42,
    })
    expect(h.applyConversationChange).toHaveBeenNthCalledWith(3, {
      kind: "state",
      patch: {
        id: 42,
        status: "pending_review",
        awaiting_reply_token: null,
        updated_at: "2026-07-19T00:00:00.000Z",
      },
    })
  })

  it("calls refetchTracked on the child projection cache on reconnect", async () => {
    await mountProvider()
    await act(async () => {
      h.reconnect?.()
    })
    expect(h.refetchTracked).toHaveBeenCalledTimes(1)
  })

  it("nudges terminal delegate detail even though child upserts stay out of the root list", async () => {
    const { useConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    const syncDelegate = vi
      .spyOn(
        useConversationRuntimeStore.getState().actions,
        "syncDelegateTerminalDetail"
      )
      .mockImplementation(() => {})
    await mountProvider()
    emit({
      kind: "upsert",
      summary: makeSummary({
        id: 42,
        kind: "delegate",
        parent_id: 1,
        delegation_task_status: "completed",
      }),
    })
    expect(syncDelegate).toHaveBeenCalledWith(42)
    expect(screen.getByTestId("ids").textContent).toBe("")
    syncDelegate.mockRestore()
  })

  it("on reconnect syncs only open terminal delegates and preserveLive-refreshes every open delegate", async () => {
    const { useConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    const runtime = useConversationRuntimeStore.getState()
    const syncDelegate = vi
      .spyOn(runtime.actions, "syncDelegateTerminalDetail")
      .mockImplementation(() => {})
    const refetchDetail = vi
      .spyOn(runtime.actions, "refetchDetail")
      .mockImplementation(() => {})

    // Seed partial runtime sessions (casts acceptable for event-routing test).
    useConversationRuntimeStore.setState({
      byConversationId: new Map([
        [
          10,
          {
            conversationId: 10,
            detail: {
              summary: makeSummary({
                id: 10,
                kind: "delegate",
                parent_id: 1,
                delegation_task_status: "completed",
              }),
            },
          } as never,
        ],
        [
          11,
          {
            conversationId: 11,
            detail: {
              summary: makeSummary({
                id: 11,
                kind: "delegate",
                parent_id: 1,
                delegation_task_status: "failed",
              }),
            },
          } as never,
        ],
        [
          12,
          {
            conversationId: 12,
            detail: {
              summary: makeSummary({
                id: 12,
                kind: "delegate",
                parent_id: 1,
                delegation_task_status: "running",
              }),
            },
          } as never,
        ],
        [
          13,
          {
            conversationId: 13,
            detail: {
              summary: makeSummary({
                id: 13,
                kind: "regular",
                status: "in_progress",
              }),
            },
          } as never,
        ],
      ]),
      conversationIdByExternalId: new Map(),
    })

    await mountProvider()
    syncDelegate.mockClear()
    refetchDetail.mockClear()

    await act(async () => {
      h.reconnect?.()
    })

    // Terminal open delegates only for convergence poll.
    expect(syncDelegate.mock.calls.map((c) => c[0]).sort()).toEqual([10, 11])
    // Every open delegate (running + terminal) gets preserved-live detail refresh.
    expect(refetchDetail.mock.calls.map((c) => c[0]).sort()).toEqual([
      10, 11, 12,
    ])
    for (const call of refetchDetail.mock.calls) {
      expect(call[1]).toEqual({ preserveLive: true })
    }
    // Root is not a delegate — neither terminal sync nor preserveLive refresh.
    expect(syncDelegate.mock.calls.some((c) => c[0] === 13)).toBe(false)
    expect(refetchDetail.mock.calls.some((c) => c[0] === 13)).toBe(false)

    syncDelegate.mockRestore()
    refetchDetail.mockRestore()
    const { resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()
  })

  it("disposes the subscription and reconnect handler on unmount", async () => {
    const { unmount } = await mountProvider()
    unmount()
    expect(h.disposeSpy).toHaveBeenCalledTimes(1)
    expect(h.reconnectUnsubSpy).toHaveBeenCalledTimes(1)
  })
})

describe("upsertFolder list routing", () => {
  it("seeds a chat folder into allFolders only — never the user-facing folders list", async () => {
    // Regression: the first chat send hands the backend-created hidden chat
    // folder to upsertFolder; putting it in `folders` rendered a "Chat" header
    // row in the sidebar until the next refetch/restart.
    await mountProvider()
    act(() => {
      useAppWorkspaceStore
        .getState()
        .upsertFolder(makeFolder({ id: 7, kind: "chat", name: "Chat" }))
    })
    expect(screen.getByTestId("folder-ids").textContent).toBe("")
    expect(screen.getByTestId("all-folder-ids")).toHaveTextContent("7")
  })

  it("seeds a regular folder into both lists", async () => {
    await mountProvider()
    act(() => {
      useAppWorkspaceStore.getState().upsertFolder(makeFolder({ id: 8 }))
    })
    expect(screen.getByTestId("folder-ids")).toHaveTextContent("8")
    expect(screen.getByTestId("all-folder-ids")).toHaveTextContent("8")
  })

  it("replaces an existing chat folder in allFolders in place", async () => {
    await mountProvider()
    const { upsertFolder } = useAppWorkspaceStore.getState()
    act(() => {
      upsertFolder(makeFolder({ id: 7, kind: "chat", name: "Chat" }))
    })
    act(() => {
      upsertFolder(makeFolder({ id: 9, kind: "chat", name: "Chat" }))
    })
    act(() => {
      upsertFolder(makeFolder({ id: 7, kind: "chat", name: "Chat 2" }))
    })
    expect(screen.getByTestId("all-folder-ids")).toHaveTextContent("7,9")
    expect(screen.getByTestId("folder-ids").textContent).toBe("")
  })
})

describe("AppWorkspaceProvider conversations://bulk-changed sync", () => {
  it("answers a batch-import nudge with one full conversation refetch", async () => {
    await mountProvider()
    expect(h.bulkHandler).toBeTypeOf("function")
    expect(h.listAll).toHaveBeenCalledTimes(1) // initial mount fetch
    await act(async () => {
      h.bulkHandler?.({ imported: 3, updated: 1, folder_ids: [7] })
    })
    expect(h.listAll).toHaveBeenCalledTimes(2)
  })

  it("disposes the bulk subscription on unmount", async () => {
    const { unmount } = await mountProvider()
    unmount()
    expect(h.bulkDisposeSpy).toHaveBeenCalledTimes(1)
  })
})

describe("AppWorkspaceProvider folder://changed sync", () => {
  it("registers a folder subscription + reconnect backstop on mount", async () => {
    await mountProvider()
    expect(h.folderHandler).toBeTypeOf("function")
    expect(h.folderReconnect).toBeTypeOf("function")
  })

  it("upserts a regular folder into both lists on a folder upsert event", async () => {
    // A headlessly-created worktree (e.g. an automation per-run worktree) must
    // land in `folders` (so a conversation inside it can be grouped/rendered)
    // and `allFolders` (cwd resolution) without a re-fetch.
    await mountProvider()
    emitFolder({ kind: "upsert", folder: makeFolder({ id: 12, parent_id: 1 }) })
    expect(screen.getByTestId("folder-ids")).toHaveTextContent("12")
    expect(screen.getByTestId("all-folder-ids")).toHaveTextContent("12")
  })

  it("updates recent agent in both folder lists from a folder upsert", async () => {
    await mountProvider()
    emitFolder({
      kind: "upsert",
      folder: makeFolder({ id: 12, last_agent_type: "gemini" }),
    })

    const state = useAppWorkspaceStore.getState()
    expect(
      state.folders.find((folder) => folder.id === 12)?.last_agent_type
    ).toBe("gemini")
    expect(
      state.allFolders.find((folder) => folder.id === 12)?.last_agent_type
    ).toBe("gemini")
  })

  it("replaces an existing folder in place on a repeat upsert", async () => {
    await mountProvider()
    emitFolder({ kind: "upsert", folder: makeFolder({ id: 12 }) })
    emitFolder({ kind: "upsert", folder: makeFolder({ id: 13 }) })
    emitFolder({
      kind: "upsert",
      folder: makeFolder({ id: 12, name: "renamed" }),
    })
    expect(screen.getByTestId("folder-ids")).toHaveTextContent("12,13")
  })

  it("drops a folder from both lists on a folder delete event", async () => {
    // A task worktree removed after its merge must leave the sidebar right
    // away — otherwise it lingers until the next full `fetchFolders` (reload).
    await mountProvider()
    emitFolder({ kind: "upsert", folder: makeFolder({ id: 12, parent_id: 1 }) })
    emitFolder({ kind: "upsert", folder: makeFolder({ id: 13 }) })
    emitFolder({ kind: "deleted", id: 12 })
    expect(screen.getByTestId("folder-ids")).toHaveTextContent("13")
    expect(screen.getByTestId("folder-ids")).not.toHaveTextContent("12")
    expect(screen.getByTestId("all-folder-ids")).not.toHaveTextContent("12")
  })

  it("ignores a delete for a folder it never knew about", async () => {
    await mountProvider()
    emitFolder({ kind: "upsert", folder: makeFolder({ id: 13 }) })
    emitFolder({ kind: "deleted", id: 99 })
    expect(screen.getByTestId("folder-ids")).toHaveTextContent("13")
  })

  it("closes the folder's tabs alongside the removal", async () => {
    // The backend only deletes PERSISTED tabs; device-local drafts would
    // otherwise stay open on a cwd that no longer exists.
    const spy = vi.spyOn(useTabStore.getState(), "closeTabsByFolder")
    await mountProvider()
    emitFolder({ kind: "upsert", folder: makeFolder({ id: 12, parent_id: 1 }) })
    emitFolder({ kind: "deleted", id: 12 })
    expect(spy).toHaveBeenCalledWith(12)
    spy.mockRestore()
  })

  it("re-fetches folders on transport reconnect (disconnect backstop)", async () => {
    await mountProvider()
    // Mount already fetched folders once.
    expect(h.listOpenFolders).toHaveBeenCalledTimes(1)
    await act(async () => {
      h.folderReconnect?.()
    })
    expect(h.listOpenFolders).toHaveBeenCalledTimes(2)
    expect(h.listAllFolders).toHaveBeenCalledTimes(2)
  })

  it("removes a folder from the open list on folder://changed close", async () => {
    await mountProvider()
    emitFolder({ kind: "upsert", folder: makeFolder({ id: 12 }) })
    await waitFor(() => {
      expect(
        useAppWorkspaceStore.getState().folders.some((f) => f.id === 12)
      ).toBe(true)
    })
    // Closed snapshot must not resurrect membership after local drop.
    h.listOpenFolders.mockResolvedValue([])
    h.listAllFolders.mockResolvedValue([makeFolder({ id: 12 })])
    emitFolder({ kind: "close", folder_id: 12, cause: "auto_empty" })
    await waitFor(() => {
      expect(
        useAppWorkspaceStore.getState().folders.some((f) => f.id === 12)
      ).toBe(false)
    })
    // all-history row may remain (v1: Close does not prune history cache).
    expect(
      useAppWorkspaceStore.getState().allFolders.some((f) => f.id === 12)
    ).toBe(true)
  })

  it("auto_empty close re-opens when a draft still targets the folder", async () => {
    await mountProvider()
    const folder = makeFolder({ id: 12 })
    emitFolder({ kind: "upsert", folder })
    await waitFor(() => {
      expect(
        useAppWorkspaceStore.getState().folders.some((f) => f.id === 12)
      ).toBe(true)
    })

    useTabStore.setState({
      rawTabs: [
        {
          id: "draft-12",
          kind: "conversation",
          conversationId: null,
          folderId: 12,
          agentType: "claude_code",
          title: "New",
          isPinned: false,
          workingDir: folder.path,
        },
      ],
      activeTabId: "draft-12",
    })

    h.listOpenFolders.mockResolvedValue([])
    h.listAllFolders.mockResolvedValue([folder])
    h.openFolderById.mockResolvedValue(folder)

    emitFolder({ kind: "close", folder_id: 12, cause: "auto_empty" })

    await waitFor(() => {
      expect(h.openFolderById).toHaveBeenCalledWith(12)
    })
    await waitFor(() => {
      expect(
        useAppWorkspaceStore.getState().folders.some((f) => f.id === 12)
      ).toBe(true)
    })
    // Draft binding is preserved (not disposed).
    expect(
      useTabStore
        .getState()
        .rawTabs.some((t) => t.conversationId == null && t.folderId === 12)
    ).toBe(true)
  })

  it("user_remove close never re-opens and disposes the draft binding", async () => {
    await mountProvider()
    const keep = makeFolder({ id: 3 })
    const gone = makeFolder({ id: 12 })
    emitFolder({ kind: "upsert", folder: keep })
    emitFolder({ kind: "upsert", folder: gone })
    await waitFor(() => {
      expect(
        useAppWorkspaceStore.getState().folders.some((f) => f.id === 12)
      ).toBe(true)
    })

    const draftTab = {
      id: "draft-12",
      kind: "conversation" as const,
      conversationId: null,
      folderId: 12,
      agentType: "claude_code" as const,
      title: "New",
      isPinned: false,
      workingDir: gone.path,
    }
    // Seed both projections so a rawTabs-only patch would leave `tabs` stale.
    useTabStore.setState({
      rawTabs: [draftTab],
      tabs: [draftTab],
      activeTabId: "draft-12",
    })

    h.listOpenFolders.mockResolvedValue([keep])
    h.listAllFolders.mockResolvedValue([keep, gone])

    emitFolder({ kind: "close", folder_id: 12, cause: "user_remove" })

    await waitFor(() => {
      expect(
        useAppWorkspaceStore.getState().folders.some((f) => f.id === 12)
      ).toBe(false)
    })
    expect(h.openFolderById).not.toHaveBeenCalled()
    // Draft must not keep targeting the user-removed folder on EITHER projection
    // (tab-store action must recompute `tabs` from `rawTabs`).
    await waitFor(() => {
      const st = useTabStore.getState()
      expect(
        st.rawTabs.some((t) => t.conversationId == null && t.folderId === 12)
      ).toBe(false)
      expect(
        st.tabs.some((t) => t.conversationId == null && t.folderId === 12)
      ).toBe(false)
    })
    const st = useTabStore.getState()
    const rawDraft = st.rawTabs.find((t) => t.id === "draft-12")
    const tabsDraft = st.tabs.find((t) => t.id === "draft-12")
    expect(rawDraft?.folderId).toBe(3)
    expect(tabsDraft?.folderId).toBe(3)
    expect(rawDraft?.workingDir).toBe(keep.path)
    expect(tabsDraft?.workingDir).toBe(keep.path)
  })

  it("stale Close after newer Upsert: close drops then authoritative open snapshot restores membership", async () => {
    // Required order: Upsert/open FIRST, then stale Close, then open-list
    // snapshot restores — not Close-first then Upsert.
    await mountProvider()
    const folder = makeFolder({ id: 12, name: "authoritative-open" })

    // 1) Newer open first.
    emitFolder({ kind: "upsert", folder })
    await waitFor(() => {
      expect(
        useAppWorkspaceStore.getState().folders.some((f) => f.id === 12)
      ).toBe(true)
    })

    // Close-triggered fenced refetch is deferred so we control when the
    // authoritative open snapshot commits (server already re-opened).
    const openSnap = deferred<FolderDetail[]>()
    const allSnap = deferred<FolderDetail[]>()
    h.listOpenFolders.mockReturnValueOnce(openSnap.promise)
    h.listAllFolders.mockReturnValueOnce(allSnap.promise)

    // 2) Stale Close after the newer open — local drop only.
    emitFolder({ kind: "close", folder_id: 12, cause: "auto_empty" })
    await waitFor(() => {
      expect(
        useAppWorkspaceStore.getState().folders.some((f) => f.id === 12)
      ).toBe(false)
    })

    // 3) Authoritative open snapshot from fenced refetch restores membership.
    await act(async () => {
      openSnap.resolve([folder])
      allSnap.resolve([folder])
      await Promise.resolve()
    })

    await waitFor(() => {
      expect(
        useAppWorkspaceStore.getState().folders.some((f) => f.id === 12)
      ).toBe(true)
    })
    expect(
      useAppWorkspaceStore.getState().folders.find((f) => f.id === 12)?.name
    ).toBe("authoritative-open")
  })

  it("auto_empty post-refetch guard restores membership after first reopen fails", async () => {
    // First ensureOpen (pre-refetch) fails; second ensureOpen (post-refetch)
    // is what restores membership — not merely that openFolderById was called.
    await mountProvider()
    const folder = makeFolder({ id: 12 })
    emitFolder({ kind: "upsert", folder })
    await waitFor(() => {
      expect(
        useAppWorkspaceStore.getState().folders.some((f) => f.id === 12)
      ).toBe(true)
    })

    useTabStore.setState({
      rawTabs: [
        {
          id: "draft-12",
          kind: "conversation",
          conversationId: null,
          folderId: 12,
          agentType: "claude_code",
          title: "New",
          isPinned: false,
          workingDir: folder.path,
        },
      ],
      activeTabId: "draft-12",
    })

    // Defer closed-snapshot refetch so the post-refetch guard cannot run until
    // after we observe the failed first attempt leaving membership closed.
    const closedOpen = deferred<FolderDetail[]>()
    const closedAll = deferred<FolderDetail[]>()
    h.listOpenFolders.mockReturnValueOnce(closedOpen.promise)
    h.listAllFolders.mockReturnValueOnce(closedAll.promise)

    // First call = pre-refetch ensureOpen (fails). Second = post-refetch guard.
    h.openFolderById
      .mockRejectedValueOnce(new Error("first pre-refetch reopen fails"))
      .mockResolvedValueOnce(
        makeFolder({ id: 12, name: "restored-by-second-guard" })
      )

    emitFolder({ kind: "close", folder_id: 12, cause: "auto_empty" })

    // First attempt ran and failed — membership still closed; second not yet.
    await waitFor(() => {
      expect(h.openFolderById).toHaveBeenCalledTimes(1)
    })
    await act(async () => {
      await Promise.resolve()
    })
    expect(h.openFolderById).toHaveBeenCalledTimes(1)
    expect(
      useAppWorkspaceStore.getState().folders.some((f) => f.id === 12)
    ).toBe(false)

    // Commit non-stale closed snapshot → post-refetch second guard runs.
    await act(async () => {
      closedOpen.resolve([])
      closedAll.resolve([folder])
      await Promise.resolve()
    })

    await waitFor(() => {
      expect(h.openFolderById).toHaveBeenCalledTimes(2)
    })
    await waitFor(() => {
      expect(
        useAppWorkspaceStore.getState().folders.some((f) => f.id === 12)
      ).toBe(true)
    })
    expect(
      useAppWorkspaceStore.getState().folders.find((f) => f.id === 12)?.name
    ).toBe("restored-by-second-guard")
  })

  it("reconnect fence discards a stale open list after a concurrent membership event", async () => {
    await mountProvider()
    const keep = makeFolder({ id: 1 })
    emitFolder({ kind: "upsert", folder: keep })
    emitFolder({ kind: "upsert", folder: makeFolder({ id: 12 }) })
    await waitFor(() => {
      expect(
        useAppWorkspaceStore.getState().folders.some((f) => f.id === 12)
      ).toBe(true)
    })

    const staleOpen = deferred<FolderDetail[]>()
    const staleAll = deferred<FolderDetail[]>()
    // First call = reconnect in-flight; subsequent close-handler refetch sees
    // post-close membership (keep only).
    h.listOpenFolders
      .mockReturnValueOnce(staleOpen.promise)
      .mockResolvedValue([keep])
    h.listAllFolders
      .mockReturnValueOnce(staleAll.promise)
      .mockResolvedValue([keep, makeFolder({ id: 12 })])

    await act(async () => {
      h.folderReconnect?.()
    })

    // Membership event while reconnect refetch is in flight.
    emitFolder({ kind: "close", folder_id: 12, cause: "auto_empty" })
    await waitFor(() => {
      expect(
        useAppWorkspaceStore.getState().folders.some((f) => f.id === 12)
      ).toBe(false)
    })

    // Stale reconnect snapshot still lists folder 12 — must not resurrect it.
    await act(async () => {
      staleOpen.resolve([keep, makeFolder({ id: 12 })])
      staleAll.resolve([keep, makeFolder({ id: 12 })])
      await Promise.resolve()
    })

    expect(
      useAppWorkspaceStore.getState().folders.some((f) => f.id === 12)
    ).toBe(false)
    expect(
      useAppWorkspaceStore.getState().folders.some((f) => f.id === 1)
    ).toBe(true)
  })

  it("disposes the folder subscription + reconnect handler on unmount", async () => {
    const { unmount } = await mountProvider()
    unmount()
    expect(h.folderDisposeSpy).toHaveBeenCalledTimes(1)
    expect(h.folderReconnectUnsubSpy).toHaveBeenCalledTimes(1)
  })
})

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (reason?: unknown) => void
  const promise = new Promise<T>((res, rej) => {
    resolve = res
    reject = rej
  })
  void promise.catch(() => {})
  return { promise, resolve, reject }
}
