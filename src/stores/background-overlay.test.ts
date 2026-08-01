/**
 * Background-overlay slice: out-of-turn transcript turns pushed by the backend
 * watcher (`background_activity` events) into the conversation runtime store.
 *
 * Covered invariants:
 *  - upsert semantics keyed by turn id (append new, replace-in-place a
 *    still-growing turn, adopt the event watermark), materializing the
 *    session when the conversation isn't loaded yet;
 *  - the watermark hand-off: a detail (re)fetch retires exactly the overlay
 *    entries its `transcript_watermark` covers — never more (silent loss),
 *    never fewer than none (duplicates linger only until covered);
 *  - timeline assembly: overlay turns render as persisted-phase entries after
 *    `detail.turns`, interleaved with `localTurns` by timestamp so a
 *    foreground exchange completed BETWEEN background turns keeps wall order;
 *  - a background-only session still cold-fetches detail (the overlay must
 *    not satisfy the "has active data" fetch skip).
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

import {
  BACKGROUND_OVERLAY_HARD_CAP,
  resetConversationRuntimeStore,
  selectTimelineTurns,
  useConversationRuntimeStore,
} from "@/stores/conversation-runtime-store"
import {
  BACKGROUND_TASK_MARKER,
  parseBackgroundTaskMarker,
} from "@/lib/background-agent"
import type { DbConversationDetail, MessageTurn } from "@/lib/types"

vi.mock("@/lib/api", () => ({
  getFolderConversation: vi.fn(),
}))

const { getFolderConversation } = await import("@/lib/api")
const mockGetFolderConversation = vi.mocked(getFolderConversation)

function turn(
  id: string,
  text: string,
  timestamp = "2026-07-07T03:47:08.000Z"
): MessageTurn {
  return {
    id,
    role: "assistant",
    blocks: [{ type: "text", text }],
    timestamp,
  }
}

function userTurn(id: string, text = id): MessageTurn {
  return {
    id,
    role: "user",
    blocks: [{ type: "text", text }],
    timestamp: "2026-07-07T03:47:00.000Z",
  }
}

function detail(
  overrides: Partial<DbConversationDetail> = {}
): DbConversationDetail {
  return {
    summary: {
      id: 7,
      folder_id: 1,
      agent_type: "claude_code",
      title: "t",
      title_locked: false,
      auto_title_finalized: false,
      status: "in_progress",
      awaiting_reply_token: null,
      kind: "regular",
      model: null,
      git_branch: null,
      external_id: "sess-7",
      message_count: 0,
      child_count: 0,
      created_at: "2026-07-07T03:40:00.000Z",
      updated_at: "2026-07-07T03:40:00.000Z",
      pinned_at: null,
    },
    turns: [],
    session_stats: null,
    ...overrides,
  }
}

function actions() {
  return useConversationRuntimeStore.getState().actions
}

function session(conversationId: number) {
  return useConversationRuntimeStore
    .getState()
    .byConversationId.get(conversationId)
}

async function flushMicrotasks() {
  await Promise.resolve()
  await Promise.resolve()
}

function deferredDetail(): {
  promise: Promise<DbConversationDetail>
  resolve: (detail: DbConversationDetail) => void
} {
  let resolve!: (detail: DbConversationDetail) => void
  const promise = new Promise<DbConversationDetail>((res) => {
    resolve = res
  })
  return { promise, resolve }
}

beforeEach(() => {
  resetConversationRuntimeStore()
  mockGetFolderConversation.mockReset()
  // A bare unconfigured mock returns `undefined` and `.then()` on it throws.
  // Default to a promise that never resolves so any call a test doesn't
  // explicitly configure is a harmless no-op; `mockResolvedValueOnce`/
  // `mockResolvedValue` calls below take priority for the invocations a test
  // does care about.
  mockGetFolderConversation.mockImplementation(() => new Promise(() => {}))
})

afterEach(() => {
  resetConversationRuntimeStore()
})

describe("APPLY_BACKGROUND_ACTIVITY", () => {
  it("materializes the session and upserts by turn id", () => {
    actions().applyBackgroundActivity(7, [turn("bg-100-0", "step one")], 100)
    expect(session(7)?.backgroundTurns).toHaveLength(1)
    expect(session(7)?.backgroundTurns[0].watermark).toBe(100)

    // A still-growing turn re-emits under the SAME id: replaced in place,
    // watermark adopted; a new id appends after it.
    actions().applyBackgroundActivity(
      7,
      [turn("bg-100-0", "step one + more"), turn("bg-100-1", "step two")],
      220
    )
    const entries = session(7)!.backgroundTurns
    expect(entries.map((e) => e.turn.id)).toEqual(["bg-100-0", "bg-100-1"])
    expect(entries[0].turn.blocks[0]).toMatchObject({
      text: "step one + more",
    })
    expect(entries.map((e) => e.watermark)).toEqual([220, 220])
  })

  it("drops the oldest entries past the hard cap (degraded-retirement backstop)", () => {
    // Simulate retirement never running (refetch failing) while autonomous
    // turns keep arriving: the overlay must stay bounded, oldest-first.
    for (let i = 0; i < BACKGROUND_OVERLAY_HARD_CAP + 5; i++) {
      actions().applyBackgroundActivity(7, [turn(`bg-x-${i}`, `t${i}`)], i)
    }
    const entries = session(7)!.backgroundTurns
    expect(entries).toHaveLength(BACKGROUND_OVERLAY_HARD_CAP)
    expect(entries[0].turn.id).toBe("bg-x-5")
    expect(entries[entries.length - 1].turn.id).toBe(
      `bg-x-${BACKGROUND_OVERLAY_HARD_CAP + 4}`
    )
  })
})

describe("watermark hand-off on FETCH_DETAIL_SUCCESS", () => {
  it("retires exactly the entries the detail's watermark covers", async () => {
    actions().applyBackgroundActivity(7, [turn("bg-1-0", "old")], 100)
    actions().applyBackgroundActivity(7, [turn("bg-1-1", "new")], 300)

    // Refetch whose parse consumed 200 bytes: covers the 100-watermark entry
    // (its content is in `detail.turns` now), NOT the 300 one.
    mockGetFolderConversation.mockResolvedValueOnce(
      detail({ transcript_watermark: 200, turns: [turn("turn-0", "old")] })
    )
    actions().refetchDetail(7)
    await flushMicrotasks()

    const entries = session(7)!.backgroundTurns
    expect(entries.map((e) => e.turn.id)).toEqual(["bg-1-1"])
  })

  it("keeps every entry when the detail carries no watermark", async () => {
    actions().applyBackgroundActivity(7, [turn("bg-1-0", "x")], 100)
    mockGetFolderConversation.mockResolvedValueOnce(detail())
    actions().refetchDetail(7)
    await flushMicrotasks()
    expect(session(7)!.backgroundTurns).toHaveLength(1)
  })

  it("preserves array identity when nothing retires", async () => {
    actions().applyBackgroundActivity(7, [turn("bg-1-0", "x")], 500)
    const before = session(7)!.backgroundTurns
    mockGetFolderConversation.mockResolvedValueOnce(
      detail({ transcript_watermark: 200 })
    )
    actions().refetchDetail(7)
    await flushMicrotasks()
    expect(session(7)!.backgroundTurns).toBe(before)
  })
})

describe("timeline assembly", () => {
  it("renders overlay turns as persisted-phase entries after detail turns", async () => {
    mockGetFolderConversation.mockResolvedValueOnce(
      detail({
        transcript_watermark: 50,
        turns: [turn("turn-0", "history", "2026-07-07T03:40:05.000Z")],
      })
    )
    actions().fetchDetail(7)
    await flushMicrotasks()
    actions().applyBackgroundActivity(7, [turn("bg-60-0", "bg reply")], 120)

    const timeline = selectTimelineTurns(
      useConversationRuntimeStore.getState(),
      7
    )
    expect(timeline.map((t) => t.turn.id)).toEqual(["turn-0", "bg-60-0"])
    expect(timeline[1].phase).toBe("persisted")
    expect(new Set(timeline.map((t) => t.key)).size).toBe(timeline.length)
  })

  it("interleaves local and background turns by timestamp", () => {
    // Background turn at T1, foreground reply promoted to localTurns at T2,
    // background turn at T3 — wall order must hold in the timeline.
    actions().applyBackgroundActivity(
      7,
      [turn("bg-0-0", "bg early", "2026-07-07T03:41:00.000Z")],
      100
    )
    actions().appendOptimisticTurn(
      7,
      {
        id: "local-user",
        role: "user",
        blocks: [{ type: "text", text: "hi" }],
        timestamp: "2026-07-07T03:42:00.000Z",
      },
      "token-1"
    )
    actions().completeTurn(7, {
      id: "live-1",
      role: "assistant",
      content: [{ type: "text", text: "fg reply" }],
      startedAt: Date.parse("2026-07-07T03:42:30.000Z"),
    })
    actions().applyBackgroundActivity(
      7,
      [turn("bg-0-1", "bg late", "2026-07-07T03:43:00.000Z")],
      200
    )

    const timeline = selectTimelineTurns(
      useConversationRuntimeStore.getState(),
      7
    )
    const ids = timeline.map((t) => t.turn.id)
    expect(ids.indexOf("bg-0-0")).toBeLessThan(ids.indexOf("local-user"))
    expect(ids.indexOf("local-user")).toBeLessThan(ids.indexOf("bg-0-1"))
  })
})

describe("cold-fetch guard", () => {
  it("a background-only session still fetches detail", () => {
    actions().applyBackgroundActivity(7, [turn("bg-1-0", "x")], 100)
    mockGetFolderConversation.mockResolvedValueOnce(detail())
    actions().fetchDetail(7)
    expect(mockGetFolderConversation).toHaveBeenCalledTimes(1)
  })
})

describe("refetchDetail DB-id resolution", () => {
  // Regression: a conversation started as a new-chat draft keeps a virtual
  // (negative) runtime key forever; the DB row created on first send has a
  // different id. The settle-driven refetch dispatches on the runtime key —
  // it must FETCH with the bound DB id, or the backend errors on the virtual
  // id and the stale local turn (async sub-agent card frozen on its launch
  // ack) never flips to the persisted terminal state.
  it("fetches with the bound DB id and replaces stale local turns under the runtime key", async () => {
    const VIRTUAL = -7
    actions().setDbConversationId(VIRTUAL, 42)

    // Foreground turn completed live: the launch card's raw wire ack sits in
    // localTurns, exactly as after COMPLETE_TURN in production.
    actions().appendOptimisticTurn(
      VIRTUAL,
      {
        id: "u-1",
        role: "user",
        blocks: [{ type: "text", text: "run build in background" }],
        timestamp: "2026-07-07T08:38:53.000Z",
      },
      "token-1"
    )
    actions().completeTurn(VIRTUAL, {
      id: "live-1",
      role: "assistant",
      content: [{ type: "text", text: "Async agent launched successfully." }],
      startedAt: Date.parse("2026-07-07T08:39:06.000Z"),
    })
    expect(session(VIRTUAL)!.localTurns.length).toBeGreaterThan(0)

    // Settled refetch: the parser has folded the task-notification into the
    // launching turn (terminal marker) by now.
    mockGetFolderConversation.mockResolvedValueOnce(
      detail({
        summary: { ...detail().summary, id: 42 },
        transcript_watermark: 35582,
        turns: [turn("turn-0", "[[codeg-background-task]] terminal state")],
      })
    )
    actions().refetchDetail(VIRTUAL, { preserveLive: false })
    await flushMicrotasks()

    // 1 call: `completeTurn` no longer fires an implicit refetch (see its
    // own comment — it raced the transcript's last write and lost content).
    expect(mockGetFolderConversation).toHaveBeenCalledTimes(1)
    expect(mockGetFolderConversation).toHaveBeenCalledWith(
      42,
      expect.any(Object)
    )
    // Result lands under the runtime key; the stale live buffers are gone and
    // the persisted (terminal) copy is what the timeline renders.
    expect(session(VIRTUAL)?.detail?.turns.map((t) => t.id)).toEqual(["turn-0"])
    expect(session(VIRTUAL)?.localTurns).toEqual([])
    const timeline = selectTimelineTurns(
      useConversationRuntimeStore.getState(),
      VIRTUAL
    )
    expect(timeline.map((t) => t.turn.id)).toEqual(["turn-0"])
  })

  it("falls back to the session key when no DB id is bound", async () => {
    mockGetFolderConversation.mockResolvedValueOnce(detail())
    actions().refetchDetail(7)
    await flushMicrotasks()
    expect(mockGetFolderConversation).toHaveBeenCalledWith(
      7,
      expect.any(Object)
    )
  })
})

describe("history page and refetch coordination", () => {
  it("preserves the loaded oldest turn when remote history advances", async () => {
    const loaded = Array.from({ length: 20 }, (_, index) =>
      userTurn(`u${index + 1}`)
    )
    mockGetFolderConversation.mockResolvedValueOnce(
      detail({
        turns: loaded,
        history_window: {
          has_more_before: true,
          total_turn_count: 40,
          total_user_turn_count: 40,
          user_turn_limit: 20,
          returned_user_turn_count: 20,
        },
      })
    )
    actions().fetchDetail(7)
    await flushMicrotasks()

    mockGetFolderConversation.mockResolvedValueOnce(
      detail({
        turns: [
          ...Array.from({ length: 19 }, (_, index) =>
            userTurn(`u${index + 2}`)
          ),
          userTurn("u21"),
        ],
        history_window: {
          has_more_before: true,
          total_turn_count: 41,
          total_user_turn_count: 41,
          user_turn_limit: 20,
          returned_user_turn_count: 20,
        },
      })
    )
    actions().refetchDetail(7)
    await flushMicrotasks()

    expect(session(7)?.detail?.turns.map((entry) => entry.id)).toEqual(
      Array.from({ length: 21 }, (_, index) => `u${index + 1}`)
    )
    expect(session(7)?.detail?.history_window).toMatchObject({
      returned_user_turn_count: 21,
      user_turn_limit: 21,
    })
  })

  it("lets a newer refetch own a pending older page without losing its depth", async () => {
    const current = detail({
      turns: [userTurn("u21"), turn("a21", "current")],
      history_window: {
        has_more_before: true,
        total_turn_count: 80,
        total_user_turn_count: 40,
        user_turn_limit: 20,
        returned_user_turn_count: 1,
      },
    })
    mockGetFolderConversation.mockResolvedValueOnce(current)
    actions().fetchDetail(7)
    await flushMicrotasks()

    const older = deferredDetail()
    const refetch = deferredDetail()
    mockGetFolderConversation
      .mockReturnValueOnce(older.promise)
      .mockReturnValueOnce(refetch.promise)

    actions().loadOlderHistory(7)
    actions().refetchDetail(7)

    expect(mockGetFolderConversation).toHaveBeenNthCalledWith(3, 7, {
      historyUserTurnLimit: 40,
    })

    older.resolve(
      detail({
        turns: [userTurn("u1"), turn("a1", "older")],
        history_window: {
          has_more_before: true,
          total_turn_count: 80,
          total_user_turn_count: 40,
          user_turn_limit: 20,
          returned_user_turn_count: 1,
        },
      })
    )
    await flushMicrotasks()
    expect(session(7)?.detail?.turns.map((entry) => entry.id)).toEqual([
      "u1",
      "a1",
      "u21",
      "a21",
    ])

    refetch.resolve(
      detail({
        turns: [
          userTurn("u1"),
          turn("a1", "older"),
          userTurn("u21"),
          turn("a21", "current"),
        ],
        history_window: {
          has_more_before: true,
          total_turn_count: 80,
          total_user_turn_count: 40,
          user_turn_limit: 40,
          returned_user_turn_count: 2,
        },
      })
    )
    await flushMicrotasks()
    expect(session(7)?.detail?.turns.map((entry) => entry.id)).toEqual([
      "u1",
      "a1",
      "u21",
      "a21",
    ])
  })

  it("invalidates an older viewer refetch before prepending a history page", async () => {
    const current = detail({
      turns: [userTurn("u21"), turn("a21", "current")],
      history_window: {
        has_more_before: true,
        total_turn_count: 80,
        total_user_turn_count: 40,
        user_turn_limit: 20,
        returned_user_turn_count: 1,
      },
    })
    mockGetFolderConversation.mockResolvedValueOnce(current)
    actions().fetchDetail(7)
    await flushMicrotasks()

    const viewerRefetch = deferredDetail()
    const older = deferredDetail()
    mockGetFolderConversation
      .mockReturnValueOnce(viewerRefetch.promise)
      .mockReturnValueOnce(older.promise)

    actions().syncViewerDetail(7)
    actions().loadOlderHistory(7)

    older.resolve(
      detail({
        turns: [userTurn("u1"), turn("a1", "older")],
        history_window: {
          has_more_before: false,
          total_turn_count: 4,
          total_user_turn_count: 2,
          user_turn_limit: 20,
          returned_user_turn_count: 1,
        },
      })
    )
    await flushMicrotasks()

    viewerRefetch.resolve(current)
    await flushMicrotasks()

    expect(session(7)?.detail?.turns.map((entry) => entry.id)).toEqual([
      "u1",
      "a1",
      "u21",
      "a21",
    ])
  })

  it("does not recreate a removed session when an older page returns late", async () => {
    mockGetFolderConversation.mockResolvedValueOnce(
      detail({
        turns: [userTurn("u21")],
        history_window: {
          has_more_before: true,
          total_turn_count: 40,
          total_user_turn_count: 40,
          user_turn_limit: 20,
          returned_user_turn_count: 1,
        },
      })
    )
    actions().fetchDetail(7)
    await flushMicrotasks()

    const older = deferredDetail()
    mockGetFolderConversation.mockReturnValueOnce(older.promise)
    actions().loadOlderHistory(7)
    actions().removeConversation(7)

    older.resolve(detail({ turns: [userTurn("u1")] }))
    await flushMicrotasks()

    expect(session(7)).toBeUndefined()
  })

  it("does not let a superseded page clear a newer page loading state", async () => {
    mockGetFolderConversation.mockResolvedValueOnce(
      detail({
        turns: [userTurn("u21")],
        history_window: {
          has_more_before: true,
          total_turn_count: 40,
          total_user_turn_count: 40,
          user_turn_limit: 20,
          returned_user_turn_count: 1,
        },
      })
    )
    actions().fetchDetail(7)
    await flushMicrotasks()

    const firstPage = deferredDetail()
    mockGetFolderConversation.mockReturnValueOnce(firstPage.promise)
    actions().loadOlderHistory(7)

    mockGetFolderConversation.mockResolvedValueOnce(
      detail({
        turns: [userTurn("u1"), userTurn("u21")],
        history_window: {
          has_more_before: true,
          total_turn_count: 40,
          total_user_turn_count: 40,
          user_turn_limit: 40,
          returned_user_turn_count: 2,
        },
      })
    )
    actions().refetchDetail(7)
    await flushMicrotasks()

    const secondPage = deferredDetail()
    mockGetFolderConversation.mockReturnValueOnce(secondPage.promise)
    actions().loadOlderHistory(7)
    expect(session(7)?.detailHistoryLoadingOlder).toBe(true)

    firstPage.resolve(detail({ turns: [userTurn("stale")] }))
    await flushMicrotasks()
    expect(session(7)?.detailHistoryLoadingOlder).toBe(true)

    secondPage.resolve(
      detail({
        turns: [userTurn("u0")],
        history_window: {
          has_more_before: false,
          total_turn_count: 3,
          total_user_turn_count: 3,
          user_turn_limit: 20,
          returned_user_turn_count: 1,
        },
      })
    )
    await flushMicrotasks()
    expect(session(7)?.detailHistoryLoadingOlder).toBe(false)
  })
})

describe("RESOLVE_BACKGROUND_TASK (in-memory launch-card flip)", () => {
  // An async sub-agent launch card: an assistant turn holding the launching
  // `Agent` tool_use plus its ack tool_result (raw wire text). This is what
  // `AgentToolCallPart` renders as "running in background" until its
  // `output_preview` becomes a `[[codeg-background-task]]` marker.
  function launchCardTurn(
    id: string,
    toolUseId: string,
    ackText = "Async agent launched successfully."
  ): MessageTurn {
    return {
      id,
      role: "assistant",
      blocks: [
        {
          type: "tool_use",
          tool_use_id: toolUseId,
          tool_name: "Agent",
          input_preview: null,
        },
        {
          type: "tool_result",
          tool_use_id: toolUseId,
          output_preview: ackText,
          is_error: false,
        },
      ],
      timestamp: "2026-07-07T03:47:00.000Z",
    }
  }

  function ackOutput(turns: MessageTurn[], toolUseId: string): string | null {
    for (const t of turns) {
      for (const b of t.blocks) {
        if (b.type === "tool_result" && b.tool_use_id === toolUseId) {
          return b.output_preview
        }
      }
    }
    return null
  }

  const settlement = {
    toolUseId: "toolu_01",
    taskId: "agent1",
    status: "completed",
    summary: "Agent finished",
    result: "Build succeeded (exit code 0).",
  }

  it("flips a launch card already promoted into localTurns immediately", () => {
    // Seed localTurns with the launch card via the optimistic→complete path.
    actions().appendOptimisticTurn(
      7,
      launchCardTurn("t-0", "toolu_01"),
      "tok-1"
    )
    actions().completeTurn(7, null)
    expect(session(7)!.localTurns).toHaveLength(1)

    actions().resolveBackgroundTask(7, settlement)

    const output = ackOutput(session(7)!.localTurns, "toolu_01")
    expect(output).toContain(BACKGROUND_TASK_MARKER)
    const parsed = parseBackgroundTaskMarker(output)
    expect(parsed).toMatchObject({
      taskId: "agent1",
      status: "completed",
      result: "Build succeeded (exit code 0).",
    })
    // Nothing queued: it applied on the spot.
    expect(session(7)!.pendingBackgroundSettlements).toEqual([])
  })

  it("queues a settlement whose launch turn hasn't promoted yet, then applies it at COMPLETE_TURN", () => {
    // Session exists (a user prompt is in flight) but the launch card is not in
    // any promotable buffer yet — it's mid-stream in liveMessage, un-patchable.
    actions().appendOptimisticTurn(
      7,
      {
        id: "u-1",
        role: "user",
        blocks: [{ type: "text", text: "run build in background" }],
        timestamp: "2026-07-07T03:46:00.000Z",
      },
      "tok-1"
    )
    actions().resolveBackgroundTask(7, settlement)
    // Not found → queued, no crash, card untouched.
    expect(session(7)!.pendingBackgroundSettlements).toHaveLength(1)

    // The launch card now arrives and the turn completes: the drain flips it.
    actions().appendOptimisticTurn(
      7,
      launchCardTurn("t-0", "toolu_01"),
      "tok-1"
    )
    actions().completeTurn(7, null)

    const output = ackOutput(session(7)!.localTurns, "toolu_01")
    expect(parseBackgroundTaskMarker(output)).toMatchObject({
      taskId: "agent1",
      status: "completed",
    })
    expect(session(7)!.pendingBackgroundSettlements).toEqual([])
  })

  it("keeps a queued settlement whose card never promotes (no worse than a stuck card)", () => {
    actions().appendOptimisticTurn(
      7,
      {
        id: "u-1",
        role: "user",
        blocks: [{ type: "text", text: "x" }],
        timestamp: "2026-07-07T03:46:00.000Z",
      },
      "tok-1"
    )
    actions().resolveBackgroundTask(7, settlement)
    // Complete a turn that does NOT carry the launch card: the settlement can't
    // apply and must survive rather than being silently dropped.
    actions().completeTurn(7, null)
    expect(session(7)!.pendingBackgroundSettlements).toHaveLength(1)
  })

  it("de-dupes a re-settle by toolUseId (resumed sub-agent notifies again)", () => {
    actions().appendOptimisticTurn(
      7,
      {
        id: "u-1",
        role: "user",
        blocks: [{ type: "text", text: "x" }],
        timestamp: "2026-07-07T03:46:00.000Z",
      },
      "tok-1"
    )
    actions().resolveBackgroundTask(7, settlement)
    actions().resolveBackgroundTask(7, { ...settlement, status: "failed" })
    const queued = session(7)!.pendingBackgroundSettlements
    expect(queued).toHaveLength(1)
    expect(queued[0].status).toBe("failed")
  })

  it("is a no-op for a conversation with no open session", () => {
    actions().resolveBackgroundTask(999, settlement)
    expect(session(999)).toBeUndefined()
  })

  it("flips a launch card that lives in cold-loaded detail.turns (resume-after-reopen)", async () => {
    // The original card is in persisted history (e.g. a resumed sub-agent whose
    // launch was in a prior, now-cold turn). It's neither optimistic nor local,
    // so the settle must reach detail.turns or the card stays stale forever.
    mockGetFolderConversation.mockResolvedValueOnce(
      detail({ turns: [launchCardTurn("t-0", "toolu_01")] })
    )
    actions().fetchDetail(7)
    await flushMicrotasks()
    expect(session(7)?.detail?.turns).toHaveLength(1)

    actions().resolveBackgroundTask(7, settlement)

    const output = ackOutput(session(7)!.detail!.turns, "toolu_01")
    expect(parseBackgroundTaskMarker(output)).toMatchObject({
      taskId: "agent1",
      status: "completed",
      result: "Build succeeded (exit code 0).",
    })
    // Matched in detail → not queued.
    expect(session(7)!.pendingBackgroundSettlements).toEqual([])
  })

  it("does not queue an already-applied settlement, so a later result is never clobbered", () => {
    // Seed the card in localTurns and flip it to the first result.
    actions().appendOptimisticTurn(
      7,
      launchCardTurn("t-0", "toolu_01"),
      "tok-1"
    )
    actions().completeTurn(7, null)
    actions().resolveBackgroundTask(7, settlement)
    expect(session(7)!.pendingBackgroundSettlements).toEqual([])

    // Idempotent re-settle (identical result): matched-but-unchanged must NOT
    // be queued — else a later COMPLETE_TURN would re-apply this stale copy.
    actions().resolveBackgroundTask(7, settlement)
    expect(session(7)!.pendingBackgroundSettlements).toEqual([])

    // A newer result applies immediately; then a subsequent turn completes and
    // its drain must find nothing stale to revert the card with.
    actions().resolveBackgroundTask(7, { ...settlement, result: "newer B" })
    expect(session(7)!.pendingBackgroundSettlements).toEqual([])
    actions().appendOptimisticTurn(
      7,
      {
        id: "u-2",
        role: "user",
        blocks: [{ type: "text", text: "next" }],
        timestamp: "2026-07-07T03:50:00.000Z",
      },
      "tok-2"
    )
    actions().completeTurn(7, null)

    const parsed = parseBackgroundTaskMarker(
      ackOutput(session(7)!.localTurns, "toolu_01")
    )
    expect(parsed?.result).toBe("newer B")
  })
})
