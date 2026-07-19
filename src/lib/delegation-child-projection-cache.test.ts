import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import type {
  ConversationChange,
  DbConversationDetail,
  DbConversationSummary,
  DelegationRuntimeStats,
} from "@/lib/types"
import {
  DelegationChildProjectionCache,
  mapSummaryToChildCardProjection,
} from "@/lib/delegation-child-projection-cache"

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

function makeDetail(
  overrides: Partial<DbConversationSummary> & { id: number }
): DbConversationDetail {
  return {
    summary: makeSummary(overrides),
    turns: [],
  }
}

const STATS: DelegationRuntimeStats = {
  started_at: "2026-07-19T00:00:00.000Z",
  tool_call_count: 3,
  edit_tool_call_count: 1,
  touched_files: [],
  touched_files_truncated: false,
  line_counts_complete: false,
}

describe("mapSummaryToChildCardProjection", () => {
  it("maps summary fields and normalizes task status", () => {
    const projection = mapSummaryToChildCardProjection(
      makeSummary({
        id: 42,
        title: "Child title",
        delegation_call_id: "task-1",
        delegation_task_status: "completed",
        delegation_error_code: null,
        delegation_started_at: "2026-07-19T00:00:00.000Z",
        delegation_finished_at: "2026-07-19T00:01:00.000Z",
        delegation_runtime_stats: STATS,
        delegation_attention_request: null,
      })
    )
    expect(projection).toEqual({
      childConversationId: 42,
      title: "Child title",
      taskId: "task-1",
      taskStatus: "completed",
      errorCode: null,
      startedAt: "2026-07-19T00:00:00.000Z",
      finishedAt: "2026-07-19T00:01:00.000Z",
      runtimeStats: STATS,
      attentionRequest: null,
      isTerminal: true,
    })
  })

  it("treats unknown status as null and non-terminal", () => {
    const projection = mapSummaryToChildCardProjection(
      makeSummary({ id: 1, delegation_task_status: "mystery" })
    )
    expect(projection.taskStatus).toBeNull()
    expect(projection.isTerminal).toBe(false)
  })

  it("maps cancelled spelling to canceled", () => {
    const projection = mapSummaryToChildCardProjection(
      makeSummary({ id: 1, delegation_task_status: "cancelled" })
    )
    expect(projection.taskStatus).toBe("canceled")
    expect(projection.isTerminal).toBe(true)
  })
})

describe("DelegationChildProjectionCache", () => {
  let fetchConversation: ReturnType<typeof vi.fn>
  let getBackendKey: ReturnType<typeof vi.fn>
  let cache: DelegationChildProjectionCache

  beforeEach(() => {
    fetchConversation = vi.fn()
    getBackendKey = vi.fn(() => "backend-a")
    cache = new DelegationChildProjectionCache({
      fetchConversation: (id) => fetchConversation(id),
      getBackendKey: () => getBackendKey() as string,
      maxSoftEntries: 4,
      maxConcurrent: 2,
    })
  })

  afterEach(() => {
    cache.reset()
  })

  it("miss returns null until fetch resolves; then get returns projection", async () => {
    let resolve!: (v: DbConversationDetail) => void
    fetchConversation.mockReturnValue(
      new Promise<DbConversationDetail>((r) => {
        resolve = r
      })
    )

    expect(cache.get(7)).toBeNull()
    cache.ensure(7)
    expect(cache.get(7)).toBeNull()
    expect(fetchConversation).toHaveBeenCalledTimes(1)
    expect(fetchConversation).toHaveBeenCalledWith(7)

    resolve(
      makeDetail({
        id: 7,
        title: "Hydrated",
        delegation_call_id: "t7",
        delegation_task_status: "running",
      })
    )
    await vi.waitFor(() => {
      expect(cache.get(7)?.title).toBe("Hydrated")
    })
    expect(cache.get(7)?.taskId).toBe("t7")
    expect(cache.get(7)?.taskStatus).toBe("running")
    expect(cache.get(7)?.isTerminal).toBe(false)
  })

  it("dedupes concurrent ensure into one in-flight fetch", async () => {
    let resolve!: (v: DbConversationDetail) => void
    fetchConversation.mockReturnValue(
      new Promise<DbConversationDetail>((r) => {
        resolve = r
      })
    )

    cache.ensure(1)
    cache.ensure(1)
    cache.ensure(1)
    expect(fetchConversation).toHaveBeenCalledTimes(1)

    resolve(makeDetail({ id: 1, title: "Once" }))
    await vi.waitFor(() => {
      expect(cache.get(1)?.title).toBe("Once")
    })
  })

  it("applies upsert for a tracked/cached id without waiting for fetch", async () => {
    fetchConversation.mockResolvedValue(
      makeDetail({
        id: 9,
        title: "From fetch",
        delegation_task_status: "running",
      })
    )
    cache.ensure(9)
    await vi.waitFor(() => {
      expect(cache.get(9)?.title).toBe("From fetch")
    })

    const change: ConversationChange = {
      kind: "upsert",
      summary: makeSummary({
        id: 9,
        title: "From upsert",
        delegation_call_id: "task-9",
        delegation_task_status: "completed",
        delegation_finished_at: "2026-07-19T01:00:00.000Z",
        delegation_runtime_stats: STATS,
      }),
    }
    cache.applyConversationChange(change)

    expect(cache.get(9)).toMatchObject({
      title: "From upsert",
      taskId: "task-9",
      taskStatus: "completed",
      isTerminal: true,
      finishedAt: "2026-07-19T01:00:00.000Z",
      runtimeStats: STATS,
    })
  })

  it("ignores late fetch after delete tombstone", async () => {
    let resolve!: (v: DbConversationDetail) => void
    fetchConversation.mockReturnValue(
      new Promise<DbConversationDetail>((r) => {
        resolve = r
      })
    )

    cache.ensure(3)
    cache.applyConversationChange({ kind: "deleted", id: 3 })
    expect(cache.get(3)).toBeNull()

    resolve(makeDetail({ id: 3, title: "Too late" }))
    // Allow microtasks from the resolved promise to settle.
    await Promise.resolve()
    await Promise.resolve()
    expect(cache.get(3)).toBeNull()
  })

  it("generation race: newer upsert wins over older in-flight fetch", async () => {
    let resolveFetch!: (v: DbConversationDetail) => void
    fetchConversation.mockReturnValue(
      new Promise<DbConversationDetail>((r) => {
        resolveFetch = r
      })
    )

    cache.ensure(5)
    cache.applyConversationChange({
      kind: "upsert",
      summary: makeSummary({ id: 5, title: "Upsert wins" }),
    })
    expect(cache.get(5)?.title).toBe("Upsert wins")

    resolveFetch(makeDetail({ id: 5, title: "Stale fetch" }))
    await Promise.resolve()
    await Promise.resolve()
    expect(cache.get(5)?.title).toBe("Upsert wins")
  })

  it("isolates entries by backend cache key", async () => {
    fetchConversation.mockResolvedValue(makeDetail({ id: 2, title: "On A" }))
    cache.ensure(2)
    await vi.waitFor(() => {
      expect(cache.get(2)?.title).toBe("On A")
    })

    getBackendKey.mockReturnValue("backend-b")
    expect(cache.get(2)).toBeNull()

    fetchConversation.mockResolvedValue(makeDetail({ id: 2, title: "On B" }))
    cache.ensure(2)
    await vi.waitFor(() => {
      expect(cache.get(2)?.title).toBe("On B")
    })

    getBackendKey.mockReturnValue("backend-a")
    expect(cache.get(2)?.title).toBe("On A")
  })

  it("retain interest keeps entry; soft eviction drops unretained overflow", async () => {
    // maxSoftEntries = 4 in this suite's cache.
    for (let id = 1; id <= 5; id++) {
      fetchConversation.mockResolvedValueOnce(
        makeDetail({ id, title: `T${id}` })
      )
      cache.ensure(id)
    }
    await vi.waitFor(() => {
      expect(cache.get(5)?.title).toBe("T5")
    })
    // Oldest soft entry (1) should have been evicted once capacity exceeded.
    expect(cache.get(1)).toBeNull()
    expect(cache.get(2)?.title).toBe("T2")

    // Retained entry survives soft overflow.
    const release = cache.retain(10)
    fetchConversation.mockResolvedValue(makeDetail({ id: 10, title: "Kept" }))
    cache.ensure(10)
    await vi.waitFor(() => {
      expect(cache.get(10)?.title).toBe("Kept")
    })
    for (let id = 20; id <= 24; id++) {
      fetchConversation.mockResolvedValueOnce(
        makeDetail({ id, title: `X${id}` })
      )
      cache.ensure(id)
    }
    await vi.waitFor(() => {
      expect(cache.get(24)?.title).toBe("X24")
    })
    expect(cache.get(10)?.title).toBe("Kept")
    release()
  })

  it("release drops interest; entry remains soft until soft-cap eviction", async () => {
    fetchConversation.mockResolvedValue(makeDetail({ id: 11, title: "Soft" }))
    const release = cache.retain(11)
    cache.ensure(11)
    await vi.waitFor(() => {
      expect(cache.get(11)?.title).toBe("Soft")
    })
    release()
    expect(cache.get(11)?.title).toBe("Soft")
  })

  it("refetchTracked only refetches interest-held ids", async () => {
    fetchConversation.mockImplementation(async (id: number) =>
      makeDetail({ id, title: `v1-${id}` })
    )
    const release1 = cache.retain(1)
    const release2 = cache.retain(2)
    cache.ensure(1)
    cache.ensure(2)
    await vi.waitFor(() => {
      expect(cache.get(1)?.title).toBe("v1-1")
      expect(cache.get(2)?.title).toBe("v1-2")
    })
    release2()
    fetchConversation.mockClear()
    fetchConversation.mockImplementation(async (id: number) =>
      makeDetail({ id, title: `v2-${id}` })
    )

    cache.refetchTracked()
    expect(fetchConversation).toHaveBeenCalledTimes(1)
    expect(fetchConversation).toHaveBeenCalledWith(1)

    await vi.waitFor(() => {
      expect(cache.get(1)?.title).toBe("v2-1")
    })
    // Soft entry for id 2 is not force-refetched.
    expect(cache.get(2)?.title).toBe("v1-2")
    release1()
  })

  it("retries once on transient fetch failure", async () => {
    fetchConversation
      .mockRejectedValueOnce(new Error("network"))
      .mockResolvedValueOnce(makeDetail({ id: 8, title: "After retry" }))

    cache.ensure(8)
    await vi.waitFor(() => {
      expect(cache.get(8)?.title).toBe("After retry")
    })
    expect(fetchConversation).toHaveBeenCalledTimes(2)
  })

  it("notifies subscribers when projection updates", async () => {
    const cb = vi.fn()
    const unsub = cache.subscribe(cb)
    fetchConversation.mockResolvedValue(makeDetail({ id: 4, title: "Notify" }))
    cache.ensure(4)
    await vi.waitFor(() => {
      expect(cb).toHaveBeenCalled()
    })
    unsub()
    cb.mockClear()
    cache.applyConversationChange({
      kind: "upsert",
      summary: makeSummary({ id: 4, title: "Again" }),
    })
    // Unsubscribed — no further notifications.
    expect(cb).not.toHaveBeenCalled()
  })

  it("state patch does not mutate projection fields", async () => {
    fetchConversation.mockResolvedValue(
      makeDetail({ id: 6, title: "Stay", delegation_task_status: "running" })
    )
    cache.ensure(6)
    await vi.waitFor(() => {
      expect(cache.get(6)?.title).toBe("Stay")
    })
    cache.applyConversationChange({
      kind: "state",
      patch: {
        id: 6,
        status: "pending_review",
        awaiting_reply_token: "tok",
        updated_at: "2026-07-19T02:00:00.000Z",
      },
    })
    expect(cache.get(6)?.title).toBe("Stay")
    expect(cache.get(6)?.taskStatus).toBe("running")
  })

  it("caps concurrent fetches and drains the queue", async () => {
    const resolvers: Array<(v: DbConversationDetail) => void> = []
    fetchConversation.mockImplementation(
      () =>
        new Promise<DbConversationDetail>((r) => {
          resolvers.push(r)
        })
    )

    // maxConcurrent = 2
    cache.ensure(100)
    cache.ensure(101)
    cache.ensure(102)
    expect(fetchConversation).toHaveBeenCalledTimes(2)

    resolvers[0]!({ ...makeDetail({ id: 100, title: "A" }) })
    await vi.waitFor(() => {
      expect(fetchConversation).toHaveBeenCalledTimes(3)
    })
    resolvers[1]!({ ...makeDetail({ id: 101, title: "B" }) })
    resolvers[2]!({ ...makeDetail({ id: 102, title: "C" }) })
    await vi.waitFor(() => {
      expect(cache.get(102)?.title).toBe("C")
    })
  })

  it("upsert for untracked unknown id is ignored", () => {
    cache.applyConversationChange({
      kind: "upsert",
      summary: makeSummary({ id: 99, title: "Ghost" }),
    })
    expect(cache.get(99)).toBeNull()
    expect(fetchConversation).not.toHaveBeenCalled()
  })

  it("upsert applies for interest-held id even before fetch lands", async () => {
    let resolve!: (v: DbConversationDetail) => void
    fetchConversation.mockReturnValue(
      new Promise<DbConversationDetail>((r) => {
        resolve = r
      })
    )
    cache.retain(12)
    cache.ensure(12)
    cache.applyConversationChange({
      kind: "upsert",
      summary: makeSummary({
        id: 12,
        title: "Early upsert",
        delegation_task_status: "running",
      }),
    })
    expect(cache.get(12)?.title).toBe("Early upsert")

    resolve(makeDetail({ id: 12, title: "Stale" }))
    await Promise.resolve()
    await Promise.resolve()
    expect(cache.get(12)?.title).toBe("Early upsert")
  })
})
