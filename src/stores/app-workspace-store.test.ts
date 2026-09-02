import { beforeEach, describe, expect, it, vi } from "vitest"
import {
  getFolderEventGeneration,
  resetAppWorkspaceStore,
  useAppWorkspaceStore,
} from "./app-workspace-store"
import type { DbConversationSummary, FolderDetail } from "@/lib/types"

const api = vi.hoisted(() => ({
  getFolder: vi.fn(),
  getGitHead: vi.fn(),
  listAllConversations: vi.fn(),
  listOpenFolderDetails: vi.fn(async () => [] as FolderDetail[]),
  listAllFolderDetails: vi.fn(async () => [] as FolderDetail[]),
  listFolderGroups: vi.fn(async () => []),
  openFolder: vi.fn(),
  openFolderById: vi.fn(),
}))

vi.mock("@/lib/api", () => ({
  getFolder: api.getFolder,
  getGitHead: api.getGitHead,
  listAllConversations: api.listAllConversations,
  listAllFolderDetails: api.listAllFolderDetails,
  listFolderGroups: api.listFolderGroups,
  listOpenFolderDetails: api.listOpenFolderDetails,
  openFolder: api.openFolder,
  openFolderById: api.openFolderById,
  openWorktreeFolder: vi.fn(),
  removeFolderFromWorkspace: vi.fn(),
  applySidebarLayout: vi.fn(),
  createFolderGroup: vi.fn(),
  updateFolderGroup: vi.fn(),
  deleteFolderGroup: vi.fn(),
  setFolderGroup: vi.fn(),
}))

const mockGetFolder = api.getFolder
const mockGetGitHead = api.getGitHead
const mockListAllFolders = api.listAllFolderDetails
const mockListOpenFolders = api.listOpenFolderDetails

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (reason?: unknown) => void
  const promise = new Promise<T>((res, rej) => {
    resolve = res
    reject = rej
  })
  // Swallow unhandled rejections from tests that reject after supersession.
  void promise.catch(() => {})
  return { promise, resolve, reject }
}

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

beforeEach(() => {
  resetAppWorkspaceStore()
  api.getFolder.mockReset()
  api.listAllConversations.mockReset()
  api.listOpenFolderDetails.mockReset()
  api.listOpenFolderDetails.mockResolvedValue([])
  api.listAllFolderDetails.mockReset()
  api.listAllFolderDetails.mockResolvedValue([])
  api.openFolderById.mockReset()
})

describe("updateConversationLocal — stats reference stability", () => {
  function seedTwo() {
    const store = useAppWorkspaceStore.getState()
    store.applyConversationUpsert(makeSummary({ id: 1, message_count: 3 }))
    store.applyConversationUpsert(makeSummary({ id: 2, message_count: 4 }))
  }

  it("reuses the stats reference on a status patch (no stat can change)", () => {
    seedTwo()
    const before = useAppWorkspaceStore.getState()
    const statsBefore = before.stats
    const conversationsBefore = before.conversations

    useAppWorkspaceStore
      .getState()
      .updateConversationLocal(1, { status: "pending_review" })

    const after = useAppWorkspaceStore.getState()
    // The regression guard: a turn-boundary status flip must NOT mint a fresh
    // `stats` object (which would re-render every stats subscriber for a no-op).
    expect(after.stats).toBe(statsBefore)
    // But the row's data genuinely changed, so `conversations` gets a new ref
    // (sidebar consumers must see the status update).
    expect(after.conversations).not.toBe(conversationsBefore)
    expect(after.conversations.find((c) => c.id === 1)?.status).toBe(
      "pending_review"
    )
  })

  it("reuses the stats reference on a title patch", () => {
    seedTwo()
    const statsBefore = useAppWorkspaceStore.getState().stats

    useAppWorkspaceStore
      .getState()
      .updateConversationLocal(2, { title: "Renamed" })

    const after = useAppWorkspaceStore.getState()
    expect(after.stats).toBe(statsBefore)
    expect(after.conversations.find((c) => c.id === 2)?.title).toBe("Renamed")
  })

  it("leaves state untouched (stable refs) for an unknown id", () => {
    seedTwo()
    const before = useAppWorkspaceStore.getState()

    before.updateConversationLocal(999, { status: "cancelled" })

    const after = useAppWorkspaceStore.getState()
    expect(after.stats).toBe(before.stats)
    expect(after.conversations).toBe(before.conversations)
  })

  it("still tracks stats when message_count actually changes (via upsert)", () => {
    seedTwo()
    // total_messages = 3 + 4
    expect(useAppWorkspaceStore.getState().stats?.total_messages).toBe(7)

    // A real message_count change flows through applyConversationUpsert (whose
    // recompute we intentionally left intact), so stats update as before.
    useAppWorkspaceStore
      .getState()
      .applyConversationUpsert(makeSummary({ id: 1, message_count: 10 }))

    expect(useAppWorkspaceStore.getState().stats?.total_messages).toBe(14)
  })
})

describe("applyConversationStatePatch — backend authority exactness", () => {
  it("applies backend conversation state without inventing updated_at", () => {
    const store = useAppWorkspaceStore.getState()
    store.applyConversationUpsert(
      makeSummary({
        id: 1,
        status: "in_progress",
        awaiting_reply_token: null,
        updated_at: "2026-07-16T01:00:00.000Z",
      })
    )
    const statsBefore = useAppWorkspaceStore.getState().stats

    store.applyConversationStatePatch({
      id: 1,
      status: "pending_review",
      awaiting_reply_token: "generation-b",
      updated_at: "2026-07-16T02:03:04.000Z",
    })

    const state = useAppWorkspaceStore.getState()
    expect(state.conversations[0]).toMatchObject({
      status: "pending_review",
      awaiting_reply_token: "generation-b",
      updated_at: "2026-07-16T02:03:04.000Z",
    })
    expect(state.stats).toBe(statsBefore)
  })

  it("ignores a state patch for an unknown conversation", () => {
    const before = useAppWorkspaceStore.getState()
    before.applyConversationStatePatch({
      id: 999,
      status: "pending_review",
      awaiting_reply_token: "unknown",
      updated_at: "2026-07-16T02:03:04.000Z",
    })
    expect(useAppWorkspaceStore.getState().conversations).toBe(
      before.conversations
    )
  })
})

describe("applyGitHead", () => {
  it("apply_git_head_updates_when_full_head_or_reference_epoch_changes_on_the_same_branch", () => {
    const store = useAppWorkspaceStore.getState()
    const first = {
      is_repo: true,
      branch: "main",
      detached: false,
      short_sha: null as string | null,
      canonical_repo: "/repo",
      head_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      reference_source_epoch: "v1:epoch-a",
    }
    store.applyGitHead(1, first)
    expect(useAppWorkspaceStore.getState().gitHeads.get(1)).toEqual(first)

    const second = {
      ...first,
      head_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      reference_source_epoch: "v1:epoch-b",
    }
    store.applyGitHead(1, second)
    expect(useAppWorkspaceStore.getState().gitHeads.get(1)).toEqual(second)
  })
})

describe("optimistic conversation activity", () => {
  it("does not invent updated_at for local title or status patches", () => {
    const store = useAppWorkspaceStore.getState()
    store.applyConversationUpsert(makeSummary({ id: 1 }))
    // Re-read after upsert: Zustand snapshots do not refresh row arrays.
    const baseline = useAppWorkspaceStore.getState().conversations[0].updated_at

    store.updateConversationLocal(1, { title: "Renamed" })
    store.updateConversationLocal(1, { status: "pending_review" })

    expect(useAppWorkspaceStore.getState().conversations[0].updated_at).toBe(
      baseline
    )
  })

  it("rolls back only the matching optimistic token", () => {
    const store = useAppWorkspaceStore.getState()
    store.applyConversationUpsert(makeSummary({ id: 1 }))
    const first = store.beginConversationActivity(1)!
    const second = store.beginConversationActivity(1)!

    store.rollbackConversationActivity(1, first)
    expect(
      useAppWorkspaceStore.getState().optimisticActivityById.get(1)?.token
    ).toBe(second)

    store.rollbackConversationActivity(1, second)
    expect(useAppWorkspaceStore.getState().optimisticActivityById.has(1)).toBe(
      false
    )
  })

  it("does not advance conversationActivitySequence on rollback", () => {
    const store = useAppWorkspaceStore.getState()
    store.applyConversationUpsert(makeSummary({ id: 1 }))
    const token = store.beginConversationActivity(1)!
    const sequence =
      useAppWorkspaceStore.getState().conversationActivitySequence

    store.rollbackConversationActivity(1, token)

    const after = useAppWorkspaceStore.getState()
    expect(after.optimisticActivityById.has(1)).toBe(false)
    expect(after.conversationActivitySequence).toBe(sequence)
  })

  it("ignores older state and acknowledges activity only past its baseline", () => {
    const store = useAppWorkspaceStore.getState()
    store.applyConversationUpsert(
      makeSummary({ id: 1, updated_at: "2026-07-18T02:00:00.000Z" })
    )
    const token = store.beginConversationActivity(1)
    expect(token).not.toBeNull()
    const sequence =
      useAppWorkspaceStore.getState().conversationActivitySequence

    store.applyConversationStatePatch({
      id: 1,
      status: "cancelled",
      awaiting_reply_token: null,
      updated_at: "2026-07-18T01:00:00.000Z",
    })
    expect(useAppWorkspaceStore.getState().conversations[0].status).toBe(
      "in_progress"
    )
    expect(useAppWorkspaceStore.getState().optimisticActivityById.has(1)).toBe(
      true
    )

    store.applyConversationStatePatch({
      id: 1,
      status: "pending_review",
      awaiting_reply_token: "generation-1",
      updated_at: "2026-07-18T03:00:00.000Z",
    })
    const after = useAppWorkspaceStore.getState()
    expect(after.optimisticActivityById.has(1)).toBe(false)
    expect(after.conversationActivitySequence).toBe(sequence + 1)
    expect(after.lastConversationActivityId).toBe(1)
  })

  it("returns null for unknown or non-root conversations", () => {
    const store = useAppWorkspaceStore.getState()
    expect(store.beginConversationActivity(999)).toBeNull()

    // Bypass root-only upsert so a child row can exist in state.
    useAppWorkspaceStore.setState({
      conversations: [makeSummary({ id: 2, parent_id: 1 })],
    })
    expect(
      useAppWorkspaceStore.getState().beginConversationActivity(2)
    ).toBeNull()
  })
})

describe("monotonic upsert and refresh reconciliation", () => {
  it("merges old upsert metadata without regressing the state tuple", () => {
    const store = useAppWorkspaceStore.getState()
    store.applyConversationUpsert(
      makeSummary({
        id: 1,
        title: "Current",
        status: "pending_review",
        awaiting_reply_token: "g2",
        updated_at: "2026-07-18T03:00:00.000Z",
      })
    )
    store.applyConversationUpsert(
      makeSummary({
        id: 1,
        title: "Metadata from old upsert",
        status: "in_progress",
        awaiting_reply_token: null,
        updated_at: "2026-07-18T02:00:00.000Z",
      })
    )

    expect(useAppWorkspaceStore.getState().conversations[0]).toMatchObject({
      title: "Metadata from old upsert",
      status: "pending_review",
      awaiting_reply_token: "g2",
      updated_at: "2026-07-18T03:00:00.000Z",
    })
  })

  it("does not let an in-flight refresh overwrite a newer event patch", async () => {
    const pending = deferred<DbConversationSummary[]>()
    api.listAllConversations.mockReturnValueOnce(pending.promise)
    const store = useAppWorkspaceStore.getState()
    store.applyConversationUpsert(
      makeSummary({ id: 1, updated_at: "2026-07-18T01:00:00.000Z" })
    )

    const refresh = store.refreshConversations()
    store.applyConversationStatePatch({
      id: 1,
      status: "pending_review",
      awaiting_reply_token: "g2",
      updated_at: "2026-07-18T03:00:00.000Z",
    })
    store.applyConversationUpsert(
      makeSummary({ id: 2, updated_at: "2026-07-18T03:01:00.000Z" })
    )
    pending.resolve([
      makeSummary({ id: 1, updated_at: "2026-07-18T01:00:00.000Z" }),
    ])
    await refresh

    const rows = useAppWorkspaceStore.getState().conversations
    // Full state tuple must survive the contended refresh merge, including
    // awaiting_reply_token from the newer authoritative patch.
    expect(rows.find((row) => row.id === 1)).toMatchObject({
      status: "pending_review",
      awaiting_reply_token: "g2",
      updated_at: "2026-07-18T03:00:00.000Z",
    })
    expect(rows.some((row) => row.id === 2)).toBe(true)
  })

  it("ignores an older refresh that resolves after a newer one", async () => {
    const first = deferred<DbConversationSummary[]>()
    const second = deferred<DbConversationSummary[]>()
    api.listAllConversations
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise)

    const store = useAppWorkspaceStore.getState()
    const refresh1 = store.refreshConversations()
    const refresh2 = store.refreshConversations()

    second.resolve([
      makeSummary({
        id: 2,
        title: "From second",
        updated_at: "2026-07-18T02:00:00.000Z",
      }),
    ])
    await refresh2

    first.resolve([
      makeSummary({
        id: 1,
        title: "From first",
        updated_at: "2026-07-18T01:00:00.000Z",
      }),
    ])
    await refresh1

    const rows = useAppWorkspaceStore.getState().conversations
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({ id: 2, title: "From second" })
    expect(useAppWorkspaceStore.getState().conversationsLoading).toBe(false)
  })

  it("does not let a superseded refresh rejection overwrite newer success loading/error state", async () => {
    const first = deferred<DbConversationSummary[]>()
    const second = deferred<DbConversationSummary[]>()
    api.listAllConversations
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise)

    const store = useAppWorkspaceStore.getState()
    const refresh1 = store.refreshConversations()
    const refresh2 = store.refreshConversations()

    second.resolve([
      makeSummary({
        id: 2,
        title: "From second",
        updated_at: "2026-07-18T02:00:00.000Z",
      }),
    ])
    await refresh2

    const afterSuccess = useAppWorkspaceStore.getState()
    expect(afterSuccess.conversationsLoading).toBe(false)
    expect(afterSuccess.conversationsError).toBeNull()
    expect(afterSuccess.conversations[0]).toMatchObject({
      id: 2,
      title: "From second",
    })

    // Older request fails after the newer one already committed success.
    first.reject(new Error("stale network failure"))
    await refresh1

    const afterStaleReject = useAppWorkspaceStore.getState()
    expect(afterStaleReject.conversationsLoading).toBe(false)
    expect(afterStaleReject.conversationsError).toBeNull()
    expect(afterStaleReject.conversations[0]).toMatchObject({
      id: 2,
      title: "From second",
    })
  })

  it("removes rows omitted by a later uncontended refresh snapshot", async () => {
    const store = useAppWorkspaceStore.getState()
    store.applyConversationUpsert(
      makeSummary({ id: 1, updated_at: "2026-07-18T01:00:00.000Z" })
    )
    store.applyConversationUpsert(
      makeSummary({ id: 2, updated_at: "2026-07-18T01:00:00.000Z" })
    )

    api.listAllConversations.mockResolvedValueOnce([
      makeSummary({ id: 1, updated_at: "2026-07-18T01:00:00.000Z" }),
    ])
    await store.refreshConversations()

    const rows = useAppWorkspaceStore.getState().conversations
    expect(rows.map((row) => row.id)).toEqual([1])
    expect(rows.some((row) => row.id === 2)).toBe(false)
  })

  it("clears optimistic activity from a newer upsert without advancing the activity sequence", () => {
    const store = useAppWorkspaceStore.getState()
    store.applyConversationUpsert(
      makeSummary({ id: 1, updated_at: "2026-07-18T02:00:00.000Z" })
    )
    expect(store.beginConversationActivity(1)).not.toBeNull()
    const sequence =
      useAppWorkspaceStore.getState().conversationActivitySequence

    store.applyConversationUpsert(
      makeSummary({ id: 1, updated_at: "2026-07-18T03:00:00.000Z" })
    )

    const after = useAppWorkspaceStore.getState()
    expect(after.optimisticActivityById.has(1)).toBe(false)
    expect(after.conversationActivitySequence).toBe(sequence)
  })

  it("clears optimistic activity from a newer refresh without advancing the activity sequence", async () => {
    const store = useAppWorkspaceStore.getState()
    store.applyConversationUpsert(
      makeSummary({ id: 1, updated_at: "2026-07-18T02:00:00.000Z" })
    )
    expect(store.beginConversationActivity(1)).not.toBeNull()
    const sequence =
      useAppWorkspaceStore.getState().conversationActivitySequence

    api.listAllConversations.mockResolvedValueOnce([
      makeSummary({ id: 1, updated_at: "2026-07-18T03:00:00.000Z" }),
    ])
    await store.refreshConversations()

    const after = useAppWorkspaceStore.getState()
    expect(after.optimisticActivityById.has(1)).toBe(false)
    expect(after.conversationActivitySequence).toBe(sequence)
    expect(after.conversations[0].updated_at).toBe("2026-07-18T03:00:00.000Z")
  })

  it("prunes optimistic activity on remove without advancing the activity sequence", () => {
    const store = useAppWorkspaceStore.getState()
    store.applyConversationUpsert(
      makeSummary({ id: 1, updated_at: "2026-07-18T02:00:00.000Z" })
    )
    expect(store.beginConversationActivity(1)).not.toBeNull()
    const sequence =
      useAppWorkspaceStore.getState().conversationActivitySequence

    store.applyConversationRemove(1)

    const after = useAppWorkspaceStore.getState()
    expect(after.optimisticActivityById.has(1)).toBe(false)
    expect(after.conversationActivitySequence).toBe(sequence)
    expect(after.conversations).toHaveLength(0)
  })

  it("never resurrects tombstoned ids from a refresh", async () => {
    const store = useAppWorkspaceStore.getState()
    store.applyConversationUpsert(
      makeSummary({ id: 1, updated_at: "2026-07-18T01:00:00.000Z" })
    )
    store.applyConversationRemove(1)

    api.listAllConversations.mockResolvedValueOnce([
      makeSummary({
        id: 1,
        title: "Stale resurrection",
        updated_at: "2026-07-18T04:00:00.000Z",
      }),
    ])
    await store.refreshConversations()

    expect(useAppWorkspaceStore.getState().conversations).toHaveLength(0)
  })

  it("does not let a stale-timestamp upsert clear optimistic activity", () => {
    const store = useAppWorkspaceStore.getState()
    store.applyConversationUpsert(
      makeSummary({
        id: 1,
        title: "Current",
        updated_at: "2026-07-18T03:00:00.000Z",
      })
    )
    expect(store.beginConversationActivity(1)).not.toBeNull()

    store.applyConversationUpsert(
      makeSummary({
        id: 1,
        title: "Stale metadata",
        updated_at: "2026-07-18T02:00:00.000Z",
      })
    )

    const after = useAppWorkspaceStore.getState()
    expect(after.conversations[0]).toMatchObject({
      title: "Stale metadata",
      updated_at: "2026-07-18T03:00:00.000Z",
    })
    expect(after.optimisticActivityById.has(1)).toBe(true)
  })
})

function makeFolder(
  overrides: Partial<FolderDetail> & { id: number }
): FolderDetail {
  return {
    name: "repo",
    path: "/tmp/repo",
    git_branch: null,
    default_agent_type: null,
    last_agent_type: null,
    last_opened_at: "2026-01-01T00:00:00.000Z",
    sort_order: 1,
    color: "#000000",
    parent_id: null,
    kind: "regular",
    alias: null,
    group_id: null,
    ...overrides,
  }
}

describe("refreshFolder — branch null-guard", () => {
  it("keeps the poll-resolved branch when the refreshed row's git_branch is null", async () => {
    // Git-head polling has populated the display branch; the folder row's
    // `git_branch` column is null (it always is today), so the refresh must
    // leave the polled name alone.
    useAppWorkspaceStore.getState().setBranch(1, "feature/x")
    mockGetFolder.mockResolvedValue(makeFolder({ id: 1, git_branch: null }))

    await useAppWorkspaceStore.getState().refreshFolder(1)

    // Regression guard for the "no branch" flash: a null DB branch must not
    // clobber the polled name (which would blank the bottom selector until the
    // next poll, up to 10s later).
    expect(useAppWorkspaceStore.getState().branches.get(1)).toBe("feature/x")
  })

  it("adopts the refreshed branch when the row actually carries one", async () => {
    useAppWorkspaceStore.getState().setBranch(1, "old")
    mockGetFolder.mockResolvedValue(makeFolder({ id: 1, git_branch: "main" }))

    await useAppWorkspaceStore.getState().refreshFolder(1)

    expect(useAppWorkspaceStore.getState().branches.get(1)).toBe("main")
  })
})

describe("folder membership generation fence", () => {
  it("dropFolderFromOpenList removes open membership only and advances generation", () => {
    const f12 = makeFolder({ id: 12 })
    const f13 = makeFolder({ id: 13 })
    useAppWorkspaceStore.setState({
      folders: [f12, f13],
      allFolders: [f12, f13],
    })
    const genBefore = getFolderEventGeneration()

    useAppWorkspaceStore.getState().dropFolderFromOpenList(12)

    const st = useAppWorkspaceStore.getState()
    expect(st.folders.map((f) => f.id)).toEqual([13])
    expect(st.allFolders.map((f) => f.id)).toEqual([12, 13])
    expect(getFolderEventGeneration()).toBeGreaterThan(genBefore)
  })

  it("discards an in-flight refetch when Close applies during the fetch", async () => {
    const open = makeFolder({ id: 1 })
    const staleOpen = deferred<FolderDetail[]>()
    const all = deferred<FolderDetail[]>()
    api.listOpenFolderDetails.mockReturnValueOnce(staleOpen.promise)
    api.listAllFolderDetails.mockReturnValueOnce(all.promise)

    useAppWorkspaceStore.setState({
      folders: [open, makeFolder({ id: 12 })],
      allFolders: [open, makeFolder({ id: 12 })],
    })

    const fetchPromise = useAppWorkspaceStore.getState().fetchFolders()
    // Close during in-flight refetch — must not be overwritten by stale open list.
    useAppWorkspaceStore.getState().dropFolderFromOpenList(12)

    staleOpen.resolve([open, makeFolder({ id: 12 })])
    all.resolve([open, makeFolder({ id: 12 })])
    await fetchPromise

    expect(
      useAppWorkspaceStore.getState().folders.some((f) => f.id === 12)
    ).toBe(false)
  })

  it("discards an in-flight refetch when Upsert applies during the fetch", async () => {
    const open = makeFolder({ id: 1 })
    const fresh = makeFolder({ id: 99, name: "fresh" })
    const staleOpen = deferred<FolderDetail[]>()
    const all = deferred<FolderDetail[]>()
    api.listOpenFolderDetails.mockReturnValueOnce(staleOpen.promise)
    api.listAllFolderDetails.mockReturnValueOnce(all.promise)

    useAppWorkspaceStore.setState({ folders: [open], allFolders: [open] })

    const fetchPromise = useAppWorkspaceStore.getState().fetchFolders()
    useAppWorkspaceStore.getState().upsertFolder(fresh)

    // Stale snapshot without folder 99 must not wipe the concurrent upsert.
    staleOpen.resolve([open])
    all.resolve([open])
    await fetchPromise

    expect(
      useAppWorkspaceStore.getState().folders.some((f) => f.id === 99)
    ).toBe(true)
  })

  it("overlapping refetches: only the latest generation-matching commit wins", async () => {
    const a = deferred<FolderDetail[]>()
    const aAll = deferred<FolderDetail[]>()
    const b = deferred<FolderDetail[]>()
    const bAll = deferred<FolderDetail[]>()

    api.listOpenFolderDetails
      .mockReturnValueOnce(a.promise)
      .mockReturnValueOnce(b.promise)
    api.listAllFolderDetails
      .mockReturnValueOnce(aAll.promise)
      .mockReturnValueOnce(bAll.promise)

    const p1 = useAppWorkspaceStore.getState().fetchFolders()
    const p2 = useAppWorkspaceStore.getState().fetchFolders()

    // First (stale) returns an empty open list; second returns folder 5.
    a.resolve([])
    aAll.resolve([])
    b.resolve([makeFolder({ id: 5 })])
    bAll.resolve([makeFolder({ id: 5 })])
    await Promise.all([p1, p2])

    expect(useAppWorkspaceStore.getState().folders.map((f) => f.id)).toEqual([
      5,
    ])
  })

  it("stale closed snapshot after newer open is discarded by the fence", async () => {
    const closedSnap = deferred<FolderDetail[]>()
    const closedAll = deferred<FolderDetail[]>()
    api.listOpenFolderDetails.mockReturnValueOnce(closedSnap.promise)
    api.listAllFolderDetails.mockReturnValueOnce(closedAll.promise)

    const fetchPromise = useAppWorkspaceStore.getState().fetchFolders()
    // User re-opened (or AutoEmpty re-open) while the closed snapshot was in flight.
    useAppWorkspaceStore
      .getState()
      .upsertFolder(makeFolder({ id: 12, name: "reopened" }))

    closedSnap.resolve([]) // stale: server still thinks 12 is closed
    closedAll.resolve([makeFolder({ id: 12 })])
    await fetchPromise

    expect(
      useAppWorkspaceStore.getState().folders.some((f) => f.id === 12)
    ).toBe(true)
  })

  it("Upsert first then Close then open snapshot: fenced refetch restores membership", async () => {
    // Same order as the context-level claim: open FIRST, then Close drop,
    // then authoritative open-list commit restores (not Close-first + Upsert).
    useAppWorkspaceStore
      .getState()
      .upsertFolder(makeFolder({ id: 12, name: "open-first" }))
    expect(
      useAppWorkspaceStore.getState().folders.some((f) => f.id === 12)
    ).toBe(true)

    // Stale Close after newer open — local membership drop only.
    useAppWorkspaceStore.getState().dropFolderFromOpenList(12)
    expect(
      useAppWorkspaceStore.getState().folders.some((f) => f.id === 12)
    ).toBe(false)

    // Post-close fenced refetch returns authoritative open snapshot (server
    // still/already has the folder open after the newer open won).
    api.listOpenFolderDetails.mockResolvedValueOnce([
      makeFolder({ id: 12, name: "authoritative-open" }),
    ])
    api.listAllFolderDetails.mockResolvedValueOnce([
      makeFolder({ id: 12, name: "authoritative-open" }),
    ])
    await useAppWorkspaceStore.getState().fetchFolders()

    expect(
      useAppWorkspaceStore.getState().folders.some((f) => f.id === 12)
    ).toBe(true)
    expect(
      useAppWorkspaceStore.getState().folders.find((f) => f.id === 12)?.name
    ).toBe("authoritative-open")
  })

  it("reconnect-style refetch fence keeps concurrent Upsert over stale empty list", async () => {
    // Mirrors onTransportReconnect → fetchFolders while a membership Upsert
    // lands mid-flight (same fence as Close).
    const staleOpen = deferred<FolderDetail[]>()
    const staleAll = deferred<FolderDetail[]>()
    api.listOpenFolderDetails.mockReturnValueOnce(staleOpen.promise)
    api.listAllFolderDetails.mockReturnValueOnce(staleAll.promise)

    useAppWorkspaceStore.setState({ folders: [], allFolders: [] })
    const reconnectFetch = useAppWorkspaceStore.getState().fetchFolders()

    useAppWorkspaceStore
      .getState()
      .upsertFolder(makeFolder({ id: 7, name: "live-upsert" }))

    staleOpen.resolve([])
    staleAll.resolve([])
    await reconnectFetch

    expect(useAppWorkspaceStore.getState().folders.map((f) => f.id)).toEqual([
      7,
    ])
  })
})

describe("applyFolderRemove", () => {
  it("drops the folder and its branch/HEAD entries from every list", () => {
    const store = useAppWorkspaceStore.getState()
    store.upsertFolder(makeFolder({ id: 1 }))
    store.upsertFolder(makeFolder({ id: 2, parent_id: 1 }))
    store.setBranch(2, "task/7")
    store.applyGitHead(2, {
      is_repo: true,
      branch: "task/7",
      detached: false,
      short_sha: "abc1234",
    })

    useAppWorkspaceStore.getState().applyFolderRemove(2)

    const after = useAppWorkspaceStore.getState()
    expect(after.folders.map((f) => f.id)).toEqual([1])
    expect(after.allFolders.map((f) => f.id)).toEqual([1])
    // Stale branch/HEAD entries would resurface if the id were ever reused.
    expect(after.branches.has(2)).toBe(false)
    expect(after.gitHeads.has(2)).toBe(false)
  })

  it("writes nothing for an unknown id (stable refs, no re-render)", () => {
    useAppWorkspaceStore.getState().upsertFolder(makeFolder({ id: 1 }))
    const before = useAppWorkspaceStore.getState()

    useAppWorkspaceStore.getState().applyFolderRemove(404)

    const after = useAppWorkspaceStore.getState()
    expect(after.folders).toBe(before.folders)
    expect(after.allFolders).toBe(before.allFolders)
    expect(after.branches).toBe(before.branches)
    expect(after.gitHeads).toBe(before.gitHeads)
  })
})

describe("applyFolderRemove — in-flight fetch resurrection guard", () => {
  it("subtracts a removed folder from a snapshot that was already in flight", async () => {
    // Mount / reconnect `fetchFolders` replaces both lists wholesale. A
    // response captured BEFORE the worktree was deleted would otherwise put it
    // straight back on screen.
    const alive = [makeFolder({ id: 1 }), makeFolder({ id: 2, parent_id: 1 })]
    let release: () => void = () => {}
    const gate = new Promise<void>((resolve) => {
      release = resolve
    })
    mockListOpenFolders.mockImplementation(async () => {
      await gate
      return alive
    })
    mockListAllFolders.mockImplementation(async () => {
      await gate
      return alive
    })

    const inFlight = useAppWorkspaceStore.getState().fetchFolders()
    useAppWorkspaceStore.getState().applyFolderRemove(2)
    release()
    await inFlight

    const after = useAppWorkspaceStore.getState()
    expect(after.folders.map((f) => f.id)).toEqual([1])
    expect(after.allFolders.map((f) => f.id)).toEqual([1])
  })

  it("keeps a folder a LATER snapshot still reports (revived while disconnected)", async () => {
    // The reconnect refetch is the reconciliation, and it may be the only place
    // a revive is ever learned: folder ids are reused (a row is revived by path
    // onto the same id), so a task retried after its worktree was cleaned
    // re-creates that exact folder while the socket is down and its upsert
    // event is dropped. Filtering a snapshot requested AFTER the removal would
    // hide that folder forever — and with it every conversation inside it.
    useAppWorkspaceStore.getState().applyFolderRemove(2)

    mockListOpenFolders.mockResolvedValue([makeFolder({ id: 2 })])
    mockListAllFolders.mockResolvedValue([makeFolder({ id: 2 })])
    await useAppWorkspaceStore.getState().fetchFolders()

    expect(useAppWorkspaceStore.getState().folders.map((f) => f.id)).toEqual([
      2,
    ])
    expect(useAppWorkspaceStore.getState().allFolders.map((f) => f.id)).toEqual(
      [2]
    )
  })

  it("lets a later upsert revive the id (a retried task re-creates its worktree)", async () => {
    useAppWorkspaceStore.getState().upsertFolder(makeFolder({ id: 2 }))
    useAppWorkspaceStore.getState().applyFolderRemove(2)
    useAppWorkspaceStore.getState().upsertFolder(makeFolder({ id: 2 }))

    mockListOpenFolders.mockResolvedValue([makeFolder({ id: 2 })])
    mockListAllFolders.mockResolvedValue([makeFolder({ id: 2 })])
    await useAppWorkspaceStore.getState().fetchFolders()

    expect(useAppWorkspaceStore.getState().folders.map((f) => f.id)).toEqual([
      2,
    ])
  })

  it("still filters an in-flight snapshot when a LATER removal is pending", async () => {
    // Two removals, one before the fetch and one during it: only the second may
    // be subtracted, and the first must not smuggle its id back in.
    useAppWorkspaceStore.getState().applyFolderRemove(3)
    let release: () => void = () => {}
    const gate = new Promise<void>((resolve) => {
      release = resolve
    })
    const snapshot = [makeFolder({ id: 1 }), makeFolder({ id: 2 })]
    mockListOpenFolders.mockImplementation(async () => {
      await gate
      return snapshot
    })
    mockListAllFolders.mockImplementation(async () => {
      await gate
      return snapshot
    })

    const inFlight = useAppWorkspaceStore.getState().fetchFolders()
    useAppWorkspaceStore.getState().applyFolderRemove(2)
    release()
    await inFlight

    expect(useAppWorkspaceStore.getState().folders.map((f) => f.id)).toEqual([
      1,
    ])
  })
})

const {
  applySidebarLayout,
  createFolderGroup,
  deleteFolderGroup,
  listFolderGroups,
  setFolderGroup,
  updateFolderGroup,
} = await import("@/lib/api")

describe("folder groups — in-flight fetch ordering", () => {
  const mockListGroups = vi.mocked(listFolderGroups)
  const mockDeleteGroup = vi.mocked(deleteFolderGroup)

  beforeEach(() => {
    resetAppWorkspaceStore()
    mockListOpenFolders.mockReset().mockResolvedValue([])
    mockListAllFolders.mockReset().mockResolvedValue([])
    mockListGroups.mockReset().mockResolvedValue([])
    mockDeleteGroup.mockReset().mockResolvedValue(undefined)
  })

  it("subtracts a deleted group from a snapshot that was already in flight", async () => {
    // `fetchFolders` replaces `folderGroups` wholesale, so a response captured
    // BEFORE the delete would put the band straight back on screen — and
    // nothing would take it down again: the `deleted` broadcast has already
    // been applied and no-ops the second time.
    const group = { id: 7, name: "Work", color: "inherit", sort_order: 1 }
    let release: () => void = () => {}
    const gate = new Promise<void>((resolve) => {
      release = resolve
    })
    mockListGroups.mockImplementation(async () => {
      await gate
      return [group]
    })
    useAppWorkspaceStore.setState({ folderGroups: [group] })

    const inFlight = useAppWorkspaceStore.getState().fetchFolders()
    await useAppWorkspaceStore.getState().deleteFolderGroup(7)
    release()
    await inFlight

    expect(useAppWorkspaceStore.getState().folderGroups).toEqual([])
  })

  it("keeps a group a LATER snapshot still reports", async () => {
    // The mirror of the guard above: filtering a snapshot requested AFTER the
    // delete would hide a group that was re-created since, and the reconnect
    // refetch may be the only place that re-creation is ever learned.
    await useAppWorkspaceStore.getState().deleteFolderGroup(7)

    const revived = { id: 7, name: "Work", color: "inherit", sort_order: 1 }
    mockListGroups.mockResolvedValue([revived])
    await useAppWorkspaceStore.getState().fetchFolders()

    expect(useAppWorkspaceStore.getState().folderGroups).toEqual([revived])
  })

  it("discards a snapshot older than one already applied", async () => {
    // Every drag ends in a `layout` nudge that triggers a refetch, so two
    // fetches are routinely in flight. If the earlier one resolves last, the
    // sidebar would settle on the second-to-last order with nothing left to
    // correct it.
    const older = [{ id: 1, name: "Old", color: "inherit", sort_order: 1 }]
    const newer = [{ id: 2, name: "New", color: "inherit", sort_order: 1 }]
    let releaseFirst: () => void = () => {}
    const firstGate = new Promise<void>((resolve) => {
      releaseFirst = resolve
    })
    mockListGroups
      .mockImplementationOnce(async () => {
        await firstGate
        return older
      })
      .mockImplementationOnce(async () => newer)

    const first = useAppWorkspaceStore.getState().fetchFolders()
    await useAppWorkspaceStore.getState().fetchFolders()
    expect(useAppWorkspaceStore.getState().folderGroups).toEqual(newer)

    releaseFirst()
    await first
    expect(useAppWorkspaceStore.getState().folderGroups).toEqual(newer)
  })
})

describe("folder groups", () => {
  const mockApplyLayout = vi.mocked(applySidebarLayout)
  const mockCreateGroup = vi.mocked(createFolderGroup)
  const mockDeleteGroup = vi.mocked(deleteFolderGroup)
  const mockSetGroup = vi.mocked(setFolderGroup)
  const mockUpdateGroup = vi.mocked(updateFolderGroup)
  const mockListGroups = vi.mocked(listFolderGroups)

  /** Let the resync a failed mutation kicks off settle before asserting. */
  const flushResync = () => new Promise((resolve) => setTimeout(resolve, 0))

  beforeEach(() => {
    resetAppWorkspaceStore()
    mockListOpenFolders.mockReset().mockResolvedValue([])
    mockListAllFolders.mockReset().mockResolvedValue([])
    mockListGroups.mockReset().mockResolvedValue([])
    mockApplyLayout.mockReset().mockResolvedValue(undefined)
    mockCreateGroup.mockReset()
    mockDeleteGroup.mockReset().mockResolvedValue(undefined)
    mockSetGroup.mockReset().mockResolvedValue(undefined)
    mockUpdateGroup.mockReset().mockResolvedValue({
      id: 7,
      name: "Work",
      color: "inherit",
      sort_order: 1,
    })
  })

  it("mirrors the backend's per-container counter when applying a layout", async () => {
    const folders = [
      makeFolder({ id: 1, sort_order: 9 }),
      makeFolder({ id: 5, sort_order: 9 }),
      makeFolder({ id: 6, sort_order: 9 }),
    ]
    useAppWorkspaceStore.setState({
      folders,
      allFolders: folders,
      folderGroups: [{ id: 7, name: "Work", color: "inherit", sort_order: 9 }],
    })

    await useAppWorkspaceStore.getState().applySidebarLayout([
      { kind: "folder", id: 1, groupId: null },
      { kind: "group", id: 7, groupId: null },
      { kind: "folder", id: 5, groupId: 7 },
      { kind: "folder", id: 6, groupId: 7 },
    ])

    const byId = new Map(
      useAppWorkspaceStore.getState().folders.map((f) => [f.id, f])
    )
    // Top level: folder 1 then group 7 — one shared 1-based sequence.
    expect(byId.get(1)).toMatchObject({ sort_order: 1, group_id: null })
    expect(useAppWorkspaceStore.getState().folderGroups[0].sort_order).toBe(2)
    // Inside the group: its own 1..n.
    expect(byId.get(5)).toMatchObject({ sort_order: 1, group_id: 7 })
    expect(byId.get(6)).toMatchObject({ sort_order: 2, group_id: 7 })
  })

  it("rolls the optimistic layout back when the write fails, then resyncs", async () => {
    const folders = [makeFolder({ id: 1, sort_order: 3, group_id: null })]
    useAppWorkspaceStore.setState({ folders, allFolders: folders })
    mockApplyLayout.mockRejectedValue(new Error("boom"))
    // Nothing was written, so the server still holds the pre-drag order.
    mockListOpenFolders.mockResolvedValue(folders)
    mockListAllFolders.mockResolvedValue(folders)

    await expect(
      useAppWorkspaceStore
        .getState()
        .applySidebarLayout([{ kind: "folder", id: 1, groupId: 7 }])
    ).rejects.toThrow("boom")

    // Restored straight away, so the sidebar snaps back without waiting on a
    // round trip...
    expect(useAppWorkspaceStore.getState().folders[0]).toMatchObject({
      sort_order: 3,
      group_id: null,
    })
    // ...and the resync then confirms it against the server, which is what
    // stops the restore from erasing anything that landed mid-request.
    await flushResync()
    expect(mockListOpenFolders).toHaveBeenCalled()
    expect(useAppWorkspaceStore.getState().folders[0]).toMatchObject({
      sort_order: 3,
      group_id: null,
    })
  })

  it("inserts a created group immediately so a follow-up move can land in it", async () => {
    mockCreateGroup.mockResolvedValue({
      id: 7,
      name: "Work",
      color: "inherit",
      sort_order: 1,
    })
    await useAppWorkspaceStore.getState().createFolderGroup("Work")
    expect(useAppWorkspaceStore.getState().folderGroups).toEqual([
      { id: 7, name: "Work", color: "inherit", sort_order: 1 },
    ])
  })

  it("keeps a deleted group's folders, returning them to the top level", async () => {
    const folders = [
      makeFolder({ id: 5, group_id: 7 }),
      makeFolder({ id: 6, group_id: 7 }),
    ]
    useAppWorkspaceStore.setState({
      folders,
      allFolders: folders,
      folderGroups: [{ id: 7, name: "Work", color: "inherit", sort_order: 1 }],
    })

    await useAppWorkspaceStore.getState().deleteFolderGroup(7)

    expect(useAppWorkspaceStore.getState().folderGroups).toEqual([])
    // The folders must survive — "delete group" is not "close these folders".
    expect(useAppWorkspaceStore.getState().folders.map((f) => f.id)).toEqual([
      5, 6,
    ])
    expect(
      useAppWorkspaceStore.getState().folders.every((f) => f.group_id === null)
    ).toBe(true)
  })

  it("restores the group and its members when the delete fails", async () => {
    const folders = [makeFolder({ id: 5, group_id: 7 })]
    const group = { id: 7, name: "Work", color: "inherit", sort_order: 1 }
    useAppWorkspaceStore.setState({
      folders,
      allFolders: folders,
      folderGroups: [group],
    })
    mockDeleteGroup.mockRejectedValue(new Error("nope"))
    // The delete failed, so the server still has the group and its member.
    mockListOpenFolders.mockResolvedValue(folders)
    mockListAllFolders.mockResolvedValue(folders)
    mockListGroups.mockResolvedValue([group])

    await expect(
      useAppWorkspaceStore.getState().deleteFolderGroup(7)
    ).rejects.toThrow("nope")

    expect(useAppWorkspaceStore.getState().folderGroups).toHaveLength(1)
    expect(useAppWorkspaceStore.getState().folders[0].group_id).toBe(7)
    // The failed delete must also drop its tombstone, or the resync it kicks
    // off would filter the group right back out of the snapshot.
    await flushResync()
    expect(useAppWorkspaceStore.getState().folderGroups).toEqual([group])
  })

  it("moves one folder into and back out of a group", async () => {
    const folders = [makeFolder({ id: 5 })]
    useAppWorkspaceStore.setState({ folders, allFolders: folders })

    await useAppWorkspaceStore.getState().setFolderGroup(5, 7)
    expect(useAppWorkspaceStore.getState().folders[0].group_id).toBe(7)
    expect(mockSetGroup).toHaveBeenCalledWith(5, 7)

    await useAppWorkspaceStore.getState().setFolderGroup(5, null)
    expect(useAppWorkspaceStore.getState().folders[0].group_id).toBeNull()
  })

  it("patches only the named fields on update", async () => {
    useAppWorkspaceStore.setState({
      folderGroups: [{ id: 7, name: "Work", color: "red", sort_order: 1 }],
    })
    await useAppWorkspaceStore.getState().updateFolderGroup(7, { name: "Day" })
    // A rename must not reset the color the picker set.
    expect(useAppWorkspaceStore.getState().folderGroups[0]).toMatchObject({
      name: "Day",
      color: "red",
    })
  })

  it("applies upsert / deleted broadcasts without a refetch", () => {
    const folders = [makeFolder({ id: 5, group_id: 7 })]
    useAppWorkspaceStore.setState({
      folders,
      allFolders: folders,
      folderGroups: [{ id: 7, name: "Work", color: "inherit", sort_order: 1 }],
    })

    useAppWorkspaceStore.getState().applyFolderGroupChange({
      kind: "upsert",
      group: { id: 7, name: "Renamed", color: "blue", sort_order: 1 },
    })
    expect(useAppWorkspaceStore.getState().folderGroups[0]).toMatchObject({
      name: "Renamed",
      color: "blue",
    })

    useAppWorkspaceStore
      .getState()
      .applyFolderGroupChange({ kind: "deleted", id: 7 })
    expect(useAppWorkspaceStore.getState().folderGroups).toEqual([])
    // A peer's delete releases the members here too, so they don't render as
    // belonging to a group that no longer exists.
    expect(useAppWorkspaceStore.getState().folders[0].group_id).toBeNull()
  })

  it("answers a layout broadcast with a re-read", async () => {
    mockListOpenFolders.mockResolvedValue([makeFolder({ id: 5, group_id: 7 })])
    mockListAllFolders.mockResolvedValue([makeFolder({ id: 5, group_id: 7 })])

    useAppWorkspaceStore.getState().applyFolderGroupChange({ kind: "layout" })
    await vi.waitFor(() => {
      expect(useAppWorkspaceStore.getState().folders).toHaveLength(1)
    })
    expect(useAppWorkspaceStore.getState().folders[0].group_id).toBe(7)
  })
})

describe("ensureGitHead — HEAD for a folder the poll doesn't cover", () => {
  // The workspace polls exactly ONE folder (the active tab's), and the folder
  // row's `git_branch` column is never written — so any surface showing several
  // folders at once (a canvas board of conversation cards) had no way to learn
  // its folders' branches and rendered every chip as "no branch".
  beforeEach(() => {
    mockGetGitHead.mockReset()
  })

  it("resolves an unknown folder's HEAD into branches and gitHeads", async () => {
    mockGetGitHead.mockResolvedValue({
      is_repo: true,
      branch: "feature/x",
      detached: false,
      short_sha: "abc1234",
    })

    useAppWorkspaceStore.getState().ensureGitHead(9, "/tmp/repo")

    await vi.waitFor(() => {
      expect(useAppWorkspaceStore.getState().gitHeads.get(9)?.branch).toBe(
        "feature/x"
      )
    })
    // Both maps, because the chip reads `branches` for the label and `gitHeads`
    // for "is this a repo at all".
    expect(useAppWorkspaceStore.getState().branches.get(9)).toBe("feature/x")
    expect(mockGetGitHead).toHaveBeenCalledWith("/tmp/repo")
  })

  it("does not re-ask for a folder whose HEAD is already known", () => {
    useAppWorkspaceStore.getState().applyGitHead(9, {
      is_repo: true,
      branch: "main",
      detached: false,
      short_sha: "abc1234",
    })

    useAppWorkspaceStore.getState().ensureGitHead(9, "/tmp/repo")

    expect(mockGetGitHead).not.toHaveBeenCalled()
  })

  it("asks once when every card on a board asks at the same time", async () => {
    // A board mounts one branch chip per card. Without in-flight dedup, twenty
    // cards over one folder would be twenty `git rev-parse` round trips in a
    // single frame — and `gitHeads` cannot answer for them, because none of them
    // has resolved yet.
    let resolveHead: (value: {
      is_repo: boolean
      branch: string | null
      detached: boolean
      short_sha: string | null
    }) => void = () => {}
    mockGetGitHead.mockReturnValue(
      new Promise((resolve) => {
        resolveHead = resolve
      })
    )

    const { ensureGitHead } = useAppWorkspaceStore.getState()
    ensureGitHead(9, "/tmp/repo")
    ensureGitHead(9, "/tmp/repo")
    ensureGitHead(9, "/tmp/repo")

    expect(mockGetGitHead).toHaveBeenCalledTimes(1)

    resolveHead({
      is_repo: true,
      branch: "main",
      detached: false,
      short_sha: "abc1234",
    })
    await vi.waitFor(() => {
      expect(useAppWorkspaceStore.getState().gitHeads.has(9)).toBe(true)
    })
  })

  it("lets a failed read be retried instead of pinning the folder to 'no branch'", async () => {
    mockGetGitHead.mockRejectedValueOnce(new Error("boom"))
    const errors = vi.spyOn(console, "error").mockImplementation(() => {})

    useAppWorkspaceStore.getState().ensureGitHead(9, "/tmp/repo")
    await vi.waitFor(() => {
      expect(errors).toHaveBeenCalled()
    })
    expect(useAppWorkspaceStore.getState().gitHeads.has(9)).toBe(false)

    // The in-flight slot has to be released on failure too — otherwise a folder
    // that was mid-clone (or briefly unreachable) reads "no branch" for the rest
    // of the session, with nothing left to ask again.
    mockGetGitHead.mockResolvedValue({
      is_repo: true,
      branch: "main",
      detached: false,
      short_sha: "abc1234",
    })
    useAppWorkspaceStore.getState().ensureGitHead(9, "/tmp/repo")
    await vi.waitFor(() => {
      expect(useAppWorkspaceStore.getState().gitHeads.get(9)?.branch).toBe(
        "main"
      )
    })
    errors.mockRestore()
  })

  it("does not revive the HEAD of a folder removed while the read was in flight", async () => {
    let resolveHead: (value: {
      is_repo: boolean
      branch: string | null
      detached: boolean
      short_sha: string | null
    }) => void = () => {}
    mockGetGitHead.mockReturnValue(
      new Promise((resolve) => {
        resolveHead = resolve
      })
    )
    useAppWorkspaceStore.setState({ allFolders: [makeFolder({ id: 9 })] })

    useAppWorkspaceStore.getState().ensureGitHead(9, "/tmp/repo")
    useAppWorkspaceStore.getState().applyFolderRemove(9)
    resolveHead({
      is_repo: true,
      branch: "main",
      detached: false,
      short_sha: "abc1234",
    })

    await vi.waitFor(() => {
      expect(mockGetGitHead).toHaveBeenCalled()
    })
    expect(useAppWorkspaceStore.getState().gitHeads.has(9)).toBe(false)
  })

  it("re-reads for a folder id that was removed and revived mid-flight", async () => {
    // A retried task re-creates its worktree and `upsertFolder` revives the very
    // same row id — which also forgets the removal tombstone. So "was this id
    // removed since my request started?" cannot be the guard: by the time the
    // old read lands, the tombstone is gone and its answer describes a directory
    // that no longer exists.
    const settle: Array<
      (value: {
        is_repo: boolean
        branch: string | null
        detached: boolean
        short_sha: string | null
      }) => void
    > = []
    mockGetGitHead.mockImplementation(
      () =>
        new Promise((resolve) => {
          settle.push(resolve)
        })
    )
    useAppWorkspaceStore.setState({ allFolders: [makeFolder({ id: 9 })] })

    useAppWorkspaceStore.getState().ensureGitHead(9, "/tmp/repo")
    useAppWorkspaceStore.getState().applyFolderRemove(9)
    useAppWorkspaceStore.getState().upsertFolder(makeFolder({ id: 9 }))

    // The revived folder's chip must not be silenced by the dead read's slot.
    useAppWorkspaceStore.getState().ensureGitHead(9, "/tmp/repo")
    expect(mockGetGitHead).toHaveBeenCalledTimes(2)

    // Old read lands last and must lose: it is the departed worktree's branch.
    settle[1]({
      is_repo: true,
      branch: "task/new",
      detached: false,
      short_sha: "bbb2222",
    })
    settle[0]({
      is_repo: true,
      branch: "task/gone",
      detached: false,
      short_sha: "aaa1111",
    })

    await vi.waitFor(() => {
      expect(useAppWorkspaceStore.getState().gitHeads.get(9)?.branch).toBe(
        "task/new"
      )
    })
    // A moment for the stale resolution to have had its chance to overwrite.
    await Promise.resolve()
    expect(useAppWorkspaceStore.getState().gitHeads.get(9)?.branch).toBe(
      "task/new"
    )
  })
})
