import { beforeEach, describe, expect, it, vi } from "vitest"
import {
  resetAppWorkspaceStore,
  useAppWorkspaceStore,
} from "./app-workspace-store"
import type { DbConversationSummary } from "@/lib/types"

const api = vi.hoisted(() => ({
  listAllConversations: vi.fn(),
}))

vi.mock("@/lib/api", () => ({
  getFolder: vi.fn(),
  listAllConversations: api.listAllConversations,
  listAllFolderDetails: vi.fn(async () => []),
  listOpenFolderDetails: vi.fn(async () => []),
  openFolder: vi.fn(),
  openFolderById: vi.fn(),
  openWorktreeFolder: vi.fn(),
  removeFolderFromWorkspace: vi.fn(),
  reorderFolders: vi.fn(),
}))

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
  api.listAllConversations.mockReset()
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
