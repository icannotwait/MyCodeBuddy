import { beforeEach, describe, expect, it, vi } from "vitest"
import {
  getFolderEventGeneration,
  resetAppWorkspaceStore,
  useAppWorkspaceStore,
} from "./app-workspace-store"
import type { DbConversationSummary, FolderDetail } from "@/lib/types"

const api = vi.hoisted(() => ({
  getFolder: vi.fn(),
  listAllConversations: vi.fn(),
  listOpenFolderDetails: vi.fn(async () => [] as FolderDetail[]),
  listAllFolderDetails: vi.fn(async () => [] as FolderDetail[]),
  openFolderById: vi.fn(),
}))

vi.mock("@/lib/api", () => ({
  getFolder: api.getFolder,
  listAllConversations: api.listAllConversations,
  listAllFolderDetails: api.listAllFolderDetails,
  listOpenFolderDetails: api.listOpenFolderDetails,
  openFolder: vi.fn(),
  openFolderById: api.openFolderById,
  openWorktreeFolder: vi.fn(),
  removeFolderFromWorkspace: vi.fn(),
  reorderFolders: vi.fn(),
}))

const mockGetFolder = api.getFolder

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
