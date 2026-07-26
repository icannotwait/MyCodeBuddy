/**
 * Task 5 — Frontend cancel reconciliation coordinator (store-level).
 *
 * Design FE cases owned by Task 5: 1–10, 12–15, 16, 17, 18.
 * Not covered here: 11 (envelope ordering → Task 6), 19 (adapter cache → Task 7).
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import type {
  DbConversationDetail,
  MessageTurn,
  TurnOutcome,
} from "@/lib/types"
import type { LiveMessage } from "@/contexts/acp-connections-context"
import {
  __getCancelGenerationForTests,
  __getUserStopOwnershipForTests,
  CANCEL_RECONCILE_DELAYS_MS,
  SOFT_FENCE_AGE_OUT_MS,
  cancelDestructiveSuppress,
  enterOwnerPreserve,
  isStaleUserStopEnvelope,
  noteUserStopTurnOwnership,
  resetConversationRuntimeStore,
  useConversationRuntimeStore,
  type ConversationRuntimeSession,
} from "@/stores/conversation-runtime-store"

vi.mock("@/lib/api", () => ({
  getFolderConversation: vi.fn(),
}))

const { getFolderConversation } = await import("@/lib/api")
const mockGet = vi.mocked(getFolderConversation)

const CID = 42
const CONN = "conn-1"
const PROVIDER = "turn-provider-1"

const interruptedOutcome = (
  providerTurnId: string = PROVIDER,
  extras: Partial<TurnOutcome> = {}
): TurnOutcome => ({
  status: "interrupted",
  stop_reason: "cancelled",
  source: "user_stop",
  provider_turn_id: providerTurnId,
  ...extras,
})

function userTurn(id: string, text = id): MessageTurn {
  return {
    id,
    role: "user",
    blocks: [{ type: "text", text }],
    timestamp: "2026-07-25T00:00:00.000Z",
  }
}

function assistantTurn(
  id: string,
  text: string,
  outcome?: TurnOutcome | null
): MessageTurn {
  return {
    id,
    role: "assistant",
    blocks: text ? [{ type: "text", text }] : [],
    timestamp: "2026-07-25T00:00:01.000Z",
    ...(outcome !== undefined ? { outcome } : {}),
  }
}

function detail(
  turns: MessageTurn[],
  overrides: Partial<DbConversationDetail> = {}
): DbConversationDetail {
  return {
    summary: {
      id: CID,
      folder_id: 1,
      agent_type: "codex",
      title: "t",
      title_locked: false,
      auto_title_finalized: false,
      status: "in_progress",
      awaiting_reply_token: null,
      kind: "regular",
      model: null,
      git_branch: null,
      external_id: "sid-1",
      message_count: turns.length,
      child_count: 0,
      created_at: "2026-07-25T00:00:00.000Z",
      updated_at: "2026-07-25T00:00:00.000Z",
      pinned_at: null,
    },
    turns,
    session_stats: null,
    ...overrides,
  }
}

function liveMessage(id: string, text: string): LiveMessage {
  return {
    id,
    role: "assistant",
    content: [{ type: "text", text }],
    startedAt: 1_700_000_000_000,
  }
}

function emptySession(
  conversationId: number,
  overrides: Partial<ConversationRuntimeSession> = {}
): ConversationRuntimeSession {
  return {
    conversationId,
    externalId: "sid-1",
    dbConversationId: null,
    detail: null,
    detailLoading: false,
    detailError: null,
    acpLoadError: null,
    localTurns: [],
    backgroundTurns: [],
    pendingBackgroundSettlements: [],
    optimisticTurns: [],
    liveMessage: null,
    syncState: "idle",
    activeTurnToken: null,
    lastTurnOwned: false,
    liveOwnsActiveTurn: false,
    delegationKickoffText: null,
    sessionStats: null,
    delegationActivities: [],
    historyAssistantBaseline: null,
    pendingCleanup: false,
    delegateSyncError: null,
    pendingCancel: null,
    softFence: false,
    ownerPreserve: false,
    ...overrides,
  }
}

function seed(overrides: Partial<ConversationRuntimeSession> = {}): void {
  useConversationRuntimeStore.setState({
    byConversationId: new Map([[CID, emptySession(CID, overrides)]]),
  })
}

function session(): ConversationRuntimeSession {
  const s = useConversationRuntimeStore.getState().byConversationId.get(CID)
  if (!s) throw new Error("missing session")
  return s
}

function actions() {
  return useConversationRuntimeStore.getState().actions
}

/** Promote live/optimistic into localTurns (status-edge / COMPLETE_TURN path). */
function promote(live?: LiveMessage | null): void {
  actions().completeTurn(CID, live === undefined ? undefined : live)
}

function startCoordinator(
  opts: {
    completionSeq?: number
    providerTurnId?: string
    connectionId?: string
    conversationId?: number
  } = {}
): void {
  actions().startCancelReconcile({
    conversationId: opts.conversationId ?? CID,
    connectionId: opts.connectionId ?? CONN,
    completionSeq: opts.completionSeq ?? 1,
    providerTurnId: opts.providerTurnId ?? PROVIDER,
  })
}

function recordOutcome(
  opts: {
    completionSeq?: number
    connectionId?: string
    providerTurnId?: string
    source?: "user_stop" | null
  } = {}
): void {
  actions().recordTurnOutcome({
    conversationId: CID,
    connectionId: opts.connectionId ?? CONN,
    completionSeq: opts.completionSeq ?? 1,
    outcome: interruptedOutcome(opts.providerTurnId ?? PROVIDER, {
      source: opts.source === undefined ? "user_stop" : opts.source,
    }),
  })
}

function deferredDetail(): {
  promise: Promise<DbConversationDetail>
  resolve: (d: DbConversationDetail) => void
  reject: (e: unknown) => void
} {
  let resolve!: (d: DbConversationDetail) => void
  let reject!: (e: unknown) => void
  const promise = new Promise<DbConversationDetail>((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

beforeEach(() => {
  resetConversationRuntimeStore()
  mockGet.mockReset()
  vi.useFakeTimers()
})

afterEach(() => {
  resetConversationRuntimeStore()
  vi.useRealTimers()
})

// ── FE case 1: complete live buffer remains, reconciles without duplication ──

describe("FE1 complete live buffer reconciles without duplication", () => {
  it("keeps promoted live content until fenced detail replaces it once", async () => {
    seed({
      detail: detail([userTurn("u0"), assistantTurn("a0", "prior")]),
      optimisticTurns: [userTurn("u1")],
      liveMessage: liveMessage("live-1", "partial live"),
      syncState: "awaiting_persist",
      activeTurnToken: "tok-1",
      lastTurnOwned: false,
    })
    promote()
    recordOutcome()
    startCoordinator()

    expect(session().localTurns.some((t) => t.role === "assistant")).toBe(true)
    expect(session().pendingCancel).not.toBeNull()

    const full = detail([
      userTurn("u0"),
      assistantTurn("a0", "prior"),
      userTurn("u1"),
      assistantTurn("a1", "full persisted", interruptedOutcome()),
    ])
    mockGet.mockResolvedValueOnce(full)

    await vi.advanceTimersByTimeAsync(CANCEL_RECONCILE_DELAYS_MS[0])
    await Promise.resolve()

    const s = session()
    expect(s.pendingCancel).toBeNull()
    expect(s.localTurns).toEqual([])
    expect(s.optimisticTurns).toEqual([])
    expect(s.liveMessage).toBeNull()
    expect(s.detail?.turns.map((t) => t.id)).toEqual(["u0", "a0", "u1", "a1"])
    // source carried from live user_stop
    const matched = s.detail?.turns.find((t) => t.id === "a1")
    expect(matched?.outcome?.source).toBe("user_stop")
    expect(matched?.outcome?.provider_turn_id).toBe(PROVIDER)
    // no duplicated assistant for the cancelled turn
    const assistants = s.detail?.turns.filter((t) => t.role === "assistant")
    expect(assistants?.filter((t) => t.id === "a1")).toHaveLength(1)
  })
})

// ── FE case 2: partial live replaced by complete fenced persisted ──

describe("FE2 partial live replaced by complete fenced detail", () => {
  it("replaces partial local content with full fenced transcript", async () => {
    seed({
      localTurns: [
        userTurn("u1"),
        assistantTurn("a1", "partial…", interruptedOutcome()),
      ],
      lastTurnOwned: true,
    })
    startCoordinator()

    mockGet.mockResolvedValueOnce(
      detail([
        userTurn("u1"),
        assistantTurn(
          "a1",
          "partial… and more complete text",
          interruptedOutcome()
        ),
      ])
    )
    await vi.advanceTimersByTimeAsync(CANCEL_RECONCILE_DELAYS_MS[0])
    await Promise.resolve()

    expect(session().detail?.turns[1]?.blocks[0]).toMatchObject({
      type: "text",
      text: "partial… and more complete text",
    })
    expect(session().localTurns).toEqual([])
  })
})

// ── FE case 3: empty / marker-only buffer recovers complete persisted ──

describe("FE3 empty buffer recovers complete persisted response", () => {
  it("reconciles from empty localTurns into full fenced detail", async () => {
    seed({
      localTurns: [userTurn("u1")],
      lastTurnOwned: true,
    })
    recordOutcome()
    // outcome-only assistant should exist after record
    expect(session().localTurns.some((t) => t.role === "assistant")).toBe(true)
    startCoordinator()

    mockGet.mockResolvedValueOnce(
      detail([
        userTurn("u1"),
        assistantTurn("a1", "recovered full body", interruptedOutcome()),
      ])
    )
    await vi.advanceTimersByTimeAsync(CANCEL_RECONCILE_DELAYS_MS[0])
    await Promise.resolve()

    expect(session().detail?.turns[1]?.blocks[0]).toMatchObject({
      text: "recovered full body",
    })
    expect(session().localTurns).toEqual([])
  })
})

// ── FE case 4: pre-fence detail cannot clear/shorten local ──

describe("FE4 pre-fence detail cannot clear local content", () => {
  it("discards detail lacking matching interrupted fence and keeps local", async () => {
    const local = [
      userTurn("u1"),
      assistantTurn("a1", "keep me", interruptedOutcome()),
    ]
    seed({ localTurns: local, lastTurnOwned: true })
    startCoordinator()

    // First read: no matching fence
    mockGet.mockResolvedValueOnce(
      detail([userTurn("u1"), assistantTurn("a1", "shorter")])
    )
    await vi.advanceTimersByTimeAsync(CANCEL_RECONCILE_DELAYS_MS[0])
    await Promise.resolve()

    expect(session().localTurns).toEqual(local)
    expect(session().pendingCancel).not.toBeNull()
    expect(session().detail).toBeNull()
  })
})

// ── FE case 5: mismatched provider turn id cannot authorize ──

describe("FE5 mismatched provider_turn_id cannot authorize", () => {
  it("rejects fence with different provider id", async () => {
    const local = [
      userTurn("u1"),
      assistantTurn("a1", "local", interruptedOutcome()),
    ]
    seed({ localTurns: local, lastTurnOwned: true })
    startCoordinator({ providerTurnId: PROVIDER })

    mockGet.mockResolvedValueOnce(
      detail([
        userTurn("u1"),
        assistantTurn("a1", "wrong fence", interruptedOutcome("other-turn-id")),
      ])
    )
    await vi.advanceTimersByTimeAsync(CANCEL_RECONCILE_DELAYS_MS[0])
    await Promise.resolve()

    expect(session().localTurns).toEqual(local)
    expect(session().pendingCancel).not.toBeNull()
  })
})

// ── FE case 6: duplicate terminal events → one outcome, one coordinator ──

describe("FE6 duplicate terminal events are idempotent", () => {
  it("records one outcome and starts one coordinator for duplicate seq", async () => {
    seed({
      localTurns: [userTurn("u1"), assistantTurn("a1", "body")],
      lastTurnOwned: true,
    })
    recordOutcome({ completionSeq: 7 })
    recordOutcome({ completionSeq: 7 })
    startCoordinator({ completionSeq: 7 })
    startCoordinator({ completionSeq: 7 })

    const assistants = session().localTurns.filter(
      (t) => t.role === "assistant"
    )
    expect(assistants).toHaveLength(1)
    expect(assistants[0].outcome?.status).toBe("interrupted")
    expect(session().pendingCancel?.completionSeq).toBe(7)
    expect(session().pendingCancel?.cancelGeneration).toBeDefined()

    // Only one raw detail schedule — first attempt uses one mock call max after delay
    const d = deferredDetail()
    mockGet.mockReturnValueOnce(d.promise)
    await vi.advanceTimersByTimeAsync(CANCEL_RECONCILE_DELAYS_MS[0])
    expect(mockGet).toHaveBeenCalledTimes(1)
    d.resolve(
      detail([
        userTurn("u1"),
        assistantTurn("a1", "body", interruptedOutcome()),
      ])
    )
    await Promise.resolve()
    await Promise.resolve()
  })
})

// ── FE case 7: tab switch / viewer attachment does not lose pending key ──

describe("FE7 pending key survives viewer attachment / non-remove paths", () => {
  it("keeps pendingCancel when only external id is re-set to same binding", () => {
    seed({ localTurns: [userTurn("u1")], lastTurnOwned: true })
    startCoordinator()
    const key = session().pendingCancel
    expect(key).not.toBeNull()
    actions().setExternalId(CID, "sid-1")
    expect(session().pendingCancel).toEqual(key)
  })
})

// ── FE case 8: session remove / rebind / new prompt cancels stale ──

describe("FE8 remove, rebind, new prompt cancel coordinator", () => {
  it("clears pending key and ignores in-flight raw detail after remove", async () => {
    seed({ localTurns: [userTurn("u1")], lastTurnOwned: true })
    startCoordinator()
    const d = deferredDetail()
    mockGet.mockReturnValueOnce(d.promise)
    await vi.advanceTimersByTimeAsync(CANCEL_RECONCILE_DELAYS_MS[0])
    expect(mockGet).toHaveBeenCalledTimes(1)

    actions().removeConversation(CID)
    expect(
      useConversationRuntimeStore.getState().byConversationId.get(CID)
    ).toBeUndefined()

    d.resolve(
      detail([
        userTurn("u1"),
        assistantTurn("a1", "late", interruptedOutcome()),
      ])
    )
    await Promise.resolve()
    await Promise.resolve()
    expect(
      useConversationRuntimeStore.getState().byConversationId.get(CID)
    ).toBeUndefined()
  })

  it("clears pending key on new prompt (appendOptimisticTurn)", async () => {
    seed({
      localTurns: [
        userTurn("u1"),
        assistantTurn("a1", "cancelled", interruptedOutcome()),
      ],
      lastTurnOwned: true,
    })
    startCoordinator()
    expect(session().pendingCancel).not.toBeNull()

    actions().appendOptimisticTurn(CID, userTurn("u2"), "tok-next")
    expect(session().pendingCancel).toBeNull()
    expect(session().activeTurnToken).toBe("tok-next")
    // local cancel content retained
    expect(session().localTurns.some((t) => t.id === "a1")).toBe(true)
  })

  it("clears pending key on viewer new prompt and ignores stale reconcile (APPEND_VIEWER_USER_TURN)", async () => {
    seed({
      localTurns: [
        userTurn("u1"),
        assistantTurn("a1", "cancelled body", interruptedOutcome()),
      ],
      lastTurnOwned: false,
    })
    startCoordinator()
    const d = deferredDetail()
    mockGet.mockReturnValueOnce(d.promise)
    await vi.advanceTimersByTimeAsync(CANCEL_RECONCILE_DELAYS_MS[0])

    actions().appendViewerUserTurn(
      CID,
      userTurn("u2", "next from other client")
    )
    expect(session().pendingCancel).toBeNull()
    expect(session().optimisticTurns.some((t) => t.id === "u2")).toBe(true)

    d.resolve(
      detail([
        userTurn("u1"),
        assistantTurn("a1", "STALE FULL", interruptedOutcome()),
      ])
    )
    await Promise.resolve()
    await Promise.resolve()

    // Stale RECONCILE must not wipe the new viewer prompt
    expect(session().optimisticTurns.some((t) => t.id === "u2")).toBe(true)
    expect(session().localTurns.some((t) => t.id === "a1")).toBe(true)
    expect(session().detail?.turns?.[1]?.blocks[0]).not.toMatchObject({
      text: "STALE FULL",
    })
  })

  it("invalidates cancel on same-text content-dedup of a distinct viewer message id", async () => {
    // Pre-fence detail ends at the old user prompt (common after cancel before
    // assistant flush). Co-controller re-sends the same text under a NEW id —
    // content dedup suppresses the optimistic copy, but the fence must clear.
    const promptText = "continue"
    seed({
      detail: detail([userTurn("u1-persisted", promptText)]),
      localTurns: [
        userTurn("u1-local", promptText),
        assistantTurn("a1", "cancelled partial", interruptedOutcome()),
      ],
      lastTurnOwned: false,
    })
    startCoordinator()
    const d = deferredDetail()
    mockGet.mockReturnValueOnce(d.promise)
    await vi.advanceTimersByTimeAsync(CANCEL_RECONCILE_DELAYS_MS[0])

    actions().appendViewerUserTurn(CID, userTurn("u2-new-id", promptText))
    expect(session().pendingCancel).toBeNull()
    // Content dedup: no optimistic copy for the new id
    expect(session().optimisticTurns.some((t) => t.id === "u2-new-id")).toBe(
      false
    )

    d.resolve(
      detail([
        userTurn("u1-persisted", promptText),
        assistantTurn("a1", "STALE FULL SAME TEXT", interruptedOutcome()),
      ])
    )
    await Promise.resolve()
    await Promise.resolve()

    expect(session().pendingCancel).toBeNull()
    expect(session().localTurns.some((t) => t.id === "a1")).toBe(true)
    expect(session().localTurns[1]?.blocks[0]).toMatchObject({
      text: "cancelled partial",
    })
    expect(session().detail?.turns?.[1]?.blocks[0]).not.toMatchObject({
      text: "STALE FULL SAME TEXT",
    })
  })

  it("does not invalidate cancel on exact-id sender-echo dedup", () => {
    seed({
      detail: detail([userTurn("u1", "hello")]),
      localTurns: [
        userTurn("u1", "hello"),
        assistantTurn("a1", "cancelled", interruptedOutcome()),
      ],
      lastTurnOwned: true,
    })
    startCoordinator()
    const key = session().pendingCancel
    expect(key).not.toBeNull()
    const genBefore = __getCancelGenerationForTests(CID)
    // Same id as already-known user turn → exact-id dedup, keep fence
    actions().appendViewerUserTurn(CID, userTurn("u1", "hello"))
    expect(session().pendingCancel).toEqual(key)
    expect(__getCancelGenerationForTests(CID)).toBe(genBefore)
  })

  it("bumps cancelGeneration on pre-envelope viewer prompt (pendingCancel null)", () => {
    // Stop snapshotted ownership before any coordinator / pendingCancel exists.
    // A co-controller user_message for prompt B must still advance generation
    // so the late typed user_stop envelope for A is rejected.
    seed({
      localTurns: [userTurn("u1", "prompt A")],
      liveMessage: liveMessage("lm-a", "partial A"),
      syncState: "awaiting_persist",
      activeTurnToken: "tok-A",
      lastTurnOwned: false,
    })
    expect(session().pendingCancel).toBeNull()
    noteUserStopTurnOwnership(CID)
    const owned = __getUserStopOwnershipForTests(CID)
    expect(owned).toMatchObject({
      activeTurnToken: "tok-A",
      cancelGeneration: 0,
    })
    expect(isStaleUserStopEnvelope(CID)).toBe(false)

    const genBefore = __getCancelGenerationForTests(CID)
    actions().appendViewerUserTurn(
      CID,
      userTurn("u2", "prompt B from co-controller")
    )
    expect(session().pendingCancel).toBeNull()
    expect(session().optimisticTurns.some((t) => t.id === "u2")).toBe(true)
    expect(__getCancelGenerationForTests(CID)).toBeGreaterThan(genBefore)
    expect(isStaleUserStopEnvelope(CID)).toBe(true)
    // Ownership record remains (gen snapshot) but no longer matches current gen.
    expect(__getUserStopOwnershipForTests(CID)?.cancelGeneration).toBe(
      owned!.cancelGeneration
    )
  })

  it("migrates userStop ownership and stale-fences late envelopes after migrate", () => {
    const FROM = CID
    const TO = 99
    seed({
      localTurns: [userTurn("u1", "prompt A")],
      liveMessage: liveMessage("lm-a", "partial A"),
      syncState: "awaiting_persist",
      activeTurnToken: "tok-A",
      lastTurnOwned: true,
      externalId: "sid-migrate",
    })
    noteUserStopTurnOwnership(FROM)
    const owned = __getUserStopOwnershipForTests(FROM)
    expect(owned).toBeDefined()
    expect(isStaleUserStopEnvelope(FROM)).toBe(false)

    actions().migrateConversation(FROM, TO)

    // Session moved to TO; from-id is gone from the session map.
    expect(
      useConversationRuntimeStore.getState().byConversationId.get(FROM)
    ).toBeUndefined()
    expect(
      useConversationRuntimeStore.getState().byConversationId.get(TO)
    ).toBeDefined()

    // Ownership carried to TO (and tombstoned on FROM) so absence is not
    // treated as "current"; merged gen is past the snapshot on both ids.
    expect(__getUserStopOwnershipForTests(TO)).toMatchObject({
      activeTurnToken: "tok-A",
      cancelGeneration: owned!.cancelGeneration,
    })
    expect(__getUserStopOwnershipForTests(FROM)).toMatchObject({
      activeTurnToken: "tok-A",
      cancelGeneration: owned!.cancelGeneration,
    })
    expect(isStaleUserStopEnvelope(TO)).toBe(true)
    expect(isStaleUserStopEnvelope(FROM)).toBe(true)
    expect(__getCancelGenerationForTests(TO)).toBeGreaterThan(
      owned!.cancelGeneration
    )
  })

  it("migrate after appendOptimisticTurn keeps destination ownership fence stale", () => {
    // Realistic draft path: first prompt bumps from gen 0→1; Stop snapshots 1.
    // Independent destination +1 would make fresh to 0→1 match the snapshot
    // and accept a late envelope on the new id — merge must prevent that.
    const FROM = CID
    const TO = 1001
    seed({
      localTurns: [],
      lastTurnOwned: true,
      externalId: "sid-migrate-after-prompt",
    })
    actions().appendOptimisticTurn(
      FROM,
      userTurn("u-prompt-a", "prompt A"),
      "tok-A"
    )
    expect(__getCancelGenerationForTests(FROM)).toBe(1)
    expect(session().activeTurnToken).toBe("tok-A")
    expect(session().pendingCancel).toBeNull()

    noteUserStopTurnOwnership(FROM)
    const owned = __getUserStopOwnershipForTests(FROM)
    expect(owned).toMatchObject({
      activeTurnToken: "tok-A",
      cancelGeneration: 1,
    })
    expect(isStaleUserStopEnvelope(FROM)).toBe(false)

    actions().migrateConversation(FROM, TO)

    expect(__getUserStopOwnershipForTests(TO)?.cancelGeneration).toBe(1)
    expect(__getCancelGenerationForTests(TO)).toBeGreaterThan(1)
    expect(__getCancelGenerationForTests(TO)).not.toBe(owned!.cancelGeneration)
    expect(isStaleUserStopEnvelope(TO)).toBe(true)
    expect(isStaleUserStopEnvelope(FROM)).toBe(true)
    // Destination must not land exactly on the Stop snapshot (the 0→1 trap).
    expect(__getCancelGenerationForTests(TO)).not.toBe(1)
  })

  it("clears pending key on rebind (setExternalId) and on dbConversationId replace", () => {
    seed({
      localTurns: [userTurn("u1")],
      lastTurnOwned: true,
      dbConversationId: CID,
    })
    startCoordinator()
    actions().setExternalId(CID, "sid-rebound")
    expect(session().pendingCancel).toBeNull()

    startCoordinator({ completionSeq: 2 })
    expect(session().pendingCancel).not.toBeNull()
    // Replace an existing positive DB binding (not whole-store reset)
    actions().setDbConversationId(CID, 99)
    expect(session().pendingCancel).toBeNull()
  })
})

// ── FE case 9: final retry failure preserves local content ──

describe("FE9 final retry failure preserves local content", () => {
  it("clears key after exhaustion and keeps promoted local turns", async () => {
    const local = [
      userTurn("u1"),
      assistantTurn("a1", "promoted live", interruptedOutcome()),
    ]
    seed({ localTurns: local, lastTurnOwned: true })
    startCoordinator()

    // All three attempts lack the fence
    mockGet.mockResolvedValue(
      detail([userTurn("u1"), assistantTurn("a1", "incomplete")])
    )

    for (const delay of CANCEL_RECONCILE_DELAYS_MS) {
      await vi.advanceTimersByTimeAsync(delay)
      await Promise.resolve()
    }

    expect(session().pendingCancel).toBeNull()
    expect(session().localTurns).toEqual(local)
    expect(session().detail).toBeNull()
    expect(mockGet).toHaveBeenCalledTimes(3)
  })
})

// ── FE case 10: ordinary end_turn schedules no cancel detail read ──

describe("FE10 ordinary end_turn schedules no cancel detail read", () => {
  it("completeTurn alone never calls getFolderConversation for reconcile", async () => {
    seed({
      optimisticTurns: [userTurn("u1")],
      liveMessage: liveMessage("live-1", "done"),
      syncState: "awaiting_persist",
    })
    promote()
    await vi.advanceTimersByTimeAsync(5000)
    expect(mockGet).not.toHaveBeenCalled()
    expect(session().pendingCancel).toBeNull()
  })
})

// ── FE case 12: exclusive destructive path while pending ──

describe("FE12 exclusive destructive path while cancel pending", () => {
  it("blocks syncViewerDetail destructive FETCH_DETAIL_SUCCESS", async () => {
    seed({
      detail: detail([userTurn("u0")]),
      localTurns: [
        userTurn("u1"),
        assistantTurn("a1", "owner live", interruptedOutcome()),
      ],
      // pure viewer shape would allow sync — but owner lastTurnOwned blocks it.
      // Use a viewer session with pending cancel:
      lastTurnOwned: false,
      liveOwnsActiveTurn: false,
    })
    // Pending cancel on a pure viewer still blocks destructive commit
    startCoordinator()
    expect(session().pendingCancel).not.toBeNull()

    mockGet.mockResolvedValueOnce(
      detail([
        userTurn("u0"),
        userTurn("u1"),
        assistantTurn("a1", "viewer disk"),
      ])
    )
    actions().syncViewerDetail(CID)
    await Promise.resolve()
    await Promise.resolve()

    // Either no-op entirely, or no destructive replace of local
    expect(session().localTurns[1]?.blocks[0]).toMatchObject({
      text: "owner live",
    })
    // Destructive path must not install short disk without fence via viewer sync
    if (session().detail) {
      // if detail updated with preserveLive, local must remain
      expect(session().localTurns).not.toEqual([])
    }
  })

  it("blocks automatic refetchDetail destructive commit while pending", async () => {
    const local = [
      userTurn("u1"),
      assistantTurn("a1", "keep", interruptedOutcome()),
    ]
    seed({
      detail: detail([userTurn("u0")]),
      localTurns: local,
      lastTurnOwned: true,
    })
    startCoordinator()

    mockGet.mockResolvedValueOnce(
      detail([userTurn("u1"), assistantTurn("a1", "shorter no fence")])
    )
    actions().refetchDetail(CID)
    await Promise.resolve()
    await Promise.resolve()

    expect(session().localTurns).toEqual(local)
  })

  it("fetchDetail in-flight commit rechecks pendingCancel (deferred race)", async () => {
    // Empty cold session so fetchDetail actually issues a request.
    seed({
      detail: null,
      localTurns: [],
      optimisticTurns: [],
      liveMessage: null,
      lastTurnOwned: false,
    })
    const d = deferredDetail()
    mockGet.mockReturnValueOnce(d.promise)
    actions().fetchDetail(CID)
    expect(mockGet).toHaveBeenCalledTimes(1)

    // Fence starts while fetch is in flight; promote local cancel content.
    useConversationRuntimeStore.setState((state) => {
      const cur = state.byConversationId.get(CID)!
      const next = new Map(state.byConversationId)
      next.set(CID, {
        ...cur,
        localTurns: [
          userTurn("u1"),
          assistantTurn("a1", "keep live", interruptedOutcome()),
        ],
        lastTurnOwned: true,
        detailLoading: true,
      })
      return { byConversationId: next }
    })
    startCoordinator()
    expect(session().pendingCancel).not.toBeNull()

    d.resolve(
      detail([userTurn("u1"), assistantTurn("a1", "pre-fence partial")])
    )
    await Promise.resolve()
    await Promise.resolve()

    expect(session().localTurns[1]?.blocks[0]).toMatchObject({
      text: "keep live",
    })
    // Must not install the unfenced partial as authoritative wipe
    expect(session().detail?.turns?.[1]?.blocks[0]).not.toMatchObject({
      text: "pre-fence partial",
    })
  })

  it("delegate terminal sync in-flight commit rechecks pendingCancel", async () => {
    const local = [
      userTurn("u1"),
      assistantTurn("a1", "child live", interruptedOutcome()),
    ]
    seed({
      detail: detail(local, {
        summary: {
          ...detail(local).summary,
          kind: "delegate",
          delegation_task_status: "completed",
        },
      }),
      localTurns: local,
      lastTurnOwned: true,
      liveOwnsActiveTurn: true,
    })

    const d = deferredDetail()
    mockGet.mockReturnValue(d.promise)
    actions().syncDelegateTerminalDetail(CID)
    // First attempt is scheduled at delay 0 — advance so the request issues
    await vi.advanceTimersByTimeAsync(0)
    await Promise.resolve()
    expect(mockGet).toHaveBeenCalled()

    startCoordinator()
    expect(session().pendingCancel).not.toBeNull()

    d.resolve(
      detail([userTurn("u1"), assistantTurn("a1", "disk wipe")], {
        summary: {
          ...detail(local).summary,
          kind: "delegate",
          delegation_task_status: "completed",
        },
      })
    )
    await Promise.resolve()
    await Promise.resolve()

    expect(session().localTurns[1]?.blocks[0]).toMatchObject({
      text: "child live",
    })
  })
})

// ── FE case 13: prior assistant + empty current shell → outcome-only ──

describe("FE13 outcome-only turn without stamping prior assistant", () => {
  it("appends outcome-only assistant after trailing user, leaves prior intact", () => {
    seed({
      localTurns: [
        userTurn("u0"),
        assistantTurn("a0", "prior complete reply"),
        userTurn("u1"),
      ],
      lastTurnOwned: true,
    })
    recordOutcome()
    const turns = session().localTurns
    expect(turns[1].outcome).toBeUndefined()
    expect(turns[1].blocks[0]).toMatchObject({ text: "prior complete reply" })
    const last = turns[turns.length - 1]
    expect(last.role).toBe("assistant")
    expect(last.blocks).toEqual([])
    expect(last.outcome?.status).toBe("interrupted")
    expect(last.outcome?.provider_turn_id).toBe(PROVIDER)
  })
})

// ── FE case 14: queued next prompt cancels coordinator, retains local ──

describe("FE14 next prompt cancels coordinator and retains local cancel content", () => {
  it("cancels reconcile and keeps cancelled turn content", async () => {
    const local = [
      userTurn("u1"),
      assistantTurn("a1", "cancelled body", interruptedOutcome()),
    ]
    seed({ localTurns: local, lastTurnOwned: true })
    startCoordinator()

    const d = deferredDetail()
    mockGet.mockReturnValue(d.promise)
    await vi.advanceTimersByTimeAsync(CANCEL_RECONCILE_DELAYS_MS[0])

    actions().appendOptimisticTurn(CID, userTurn("u2"), "tok-2")
    expect(session().pendingCancel).toBeNull()

    d.resolve(
      detail([
        userTurn("u1"),
        assistantTurn("a1", "full", interruptedOutcome()),
      ])
    )
    await Promise.resolve()
    await Promise.resolve()

    // Stale reconcile must not commit after generation bump
    expect(session().localTurns.some((t) => t.id === "a1")).toBe(true)
    // Next prompt is optimistic until promotion
    expect(session().optimisticTurns.some((t) => t.id === "u2")).toBe(true)
    expect(session().pendingCancel).toBeNull()
  })
})

// ── FE case 15: Manual Reload authoritative during pending ──

describe("FE15 Manual Reload during pending is authoritative", () => {
  it("clears cancel key then installs detail via reloadDetail", async () => {
    seed({
      localTurns: [
        userTurn("u1"),
        assistantTurn("a1", "local", interruptedOutcome()),
      ],
      lastTurnOwned: true,
      dbConversationId: null,
    })
    startCoordinator()
    expect(session().pendingCancel).not.toBeNull()

    const reloaded = detail([
      userTurn("u1"),
      assistantTurn("a1", "from reload", interruptedOutcome()),
    ])
    mockGet.mockResolvedValueOnce(reloaded)
    actions().reloadDetail(CID, { reason: "manual_reload" })
    expect(session().pendingCancel).toBeNull()

    await Promise.resolve()
    await Promise.resolve()

    expect(session().detail?.turns[1]?.blocks[0]).toMatchObject({
      text: "from reload",
    })
    expect(session().localTurns).toEqual([])
  })

  it("resolves negative runtime id via dbConversationId map", async () => {
    const runtimeId = -7
    useConversationRuntimeStore.setState({
      byConversationId: new Map([
        [
          runtimeId,
          emptySession(runtimeId, {
            dbConversationId: CID,
            localTurns: [userTurn("u1")],
            lastTurnOwned: true,
          }),
        ],
      ]),
    })
    actions().startCancelReconcile({
      conversationId: runtimeId,
      connectionId: CONN,
      completionSeq: 1,
      providerTurnId: PROVIDER,
    })
    expect(
      useConversationRuntimeStore.getState().byConversationId.get(runtimeId)
        ?.pendingCancel
    ).not.toBeNull()

    mockGet.mockResolvedValueOnce(
      detail([userTurn("u1"), assistantTurn("a1", "ok", interruptedOutcome())])
    )
    actions().reloadDetail(runtimeId, { reason: "manual_reload" })
    await Promise.resolve()
    await Promise.resolve()

    expect(mockGet).toHaveBeenCalledWith(CID)
    const s = useConversationRuntimeStore
      .getState()
      .byConversationId.get(runtimeId)
    expect(s?.pendingCancel).toBeNull()
    expect(s?.detail?.turns).toHaveLength(2)
  })
})

// ── FE case 16: syncTurnMetadata neither cancels nor satisfies ──

describe("FE16 syncTurnMetadata does not cancel or satisfy coordinator", () => {
  it("leaves pendingCancel and does not apply via metadata path", async () => {
    seed({
      detail: detail([userTurn("u0")]),
      localTurns: [
        userTurn("u1"),
        assistantTurn("a1", "local", interruptedOutcome()),
      ],
      lastTurnOwned: true,
      historyAssistantBaseline: 0,
    })
    startCoordinator()
    const key = session().pendingCancel
    expect(key).not.toBeNull()

    mockGet.mockResolvedValueOnce(
      detail([
        userTurn("u1"),
        assistantTurn("a1", "meta path", interruptedOutcome()),
      ])
    )
    const cancelMeta = actions().syncTurnMetadata(CID)
    await Promise.resolve()
    await Promise.resolve()
    cancelMeta()

    expect(session().pendingCancel).toEqual(key)
    // metadata must not clear promoted content
    expect(session().localTurns[1]?.blocks[0]).toMatchObject({ text: "local" })
  })
})

// ── FE case 17: competing cancel generations cannot commit stale ──

describe("FE17 competing cancel generations cannot commit stale results", () => {
  it("ignores raw detail from an older cancel generation", async () => {
    seed({
      localTurns: [
        userTurn("u1"),
        assistantTurn("a1", "v1", interruptedOutcome()),
      ],
      lastTurnOwned: true,
    })
    startCoordinator({ completionSeq: 1 })
    const gen1 = session().pendingCancel!.cancelGeneration

    const stale = deferredDetail()
    mockGet.mockReturnValueOnce(stale.promise)
    await vi.advanceTimersByTimeAsync(CANCEL_RECONCILE_DELAYS_MS[0])

    // New cancel generation (e.g. clear + restart or new prompt then new cancel)
    actions().clearCancelReconcile(CID)
    expect(session().pendingCancel).toBeNull()

    seed({
      localTurns: [
        userTurn("u1"),
        assistantTurn("a1", "v1", interruptedOutcome()),
      ],
      lastTurnOwned: true,
    })
    startCoordinator({ completionSeq: 2 })
    expect(session().pendingCancel!.cancelGeneration).toBeGreaterThan(gen1)

    // Late gen1 response arrives with matching fence for PROVIDER
    stale.resolve(
      detail([
        userTurn("u1"),
        assistantTurn("a1", "STALE FULL", interruptedOutcome()),
      ])
    )
    await Promise.resolve()
    await Promise.resolve()

    // Must not apply stale generation's detail as reconcile for current session
    // (either still pending gen2, or local still "v1" if not yet reconciled)
    expect(session().localTurns[1]?.blocks[0]).toMatchObject({ text: "v1" })
    expect(session().detail?.turns?.[1]?.blocks[0]).not.toMatchObject({
      text: "STALE FULL",
    })
  })
})

// ── FE case 18: after cleanup, ordinary destructive sync is eligible ──

describe("FE18 key cleanup resumes ordinary sync eligibility", () => {
  it("keeps ownerPreserve suppress after retry exhaustion (no auto-destructive)", async () => {
    // Design: exhaustion enters owner_preserve — ordinary destructive sync
    // stays suppressed until Manual Reload / new prompt / remove / identity reset.
    seed({
      detail: detail([userTurn("u0")]),
      localTurns: [
        userTurn("u1"),
        assistantTurn("a1", "local", interruptedOutcome()),
      ],
      lastTurnOwned: true,
    })
    startCoordinator()
    mockGet.mockResolvedValue(
      detail([userTurn("u1"), assistantTurn("a1", "no fence")])
    )
    for (const delay of CANCEL_RECONCILE_DELAYS_MS) {
      await vi.advanceTimersByTimeAsync(delay)
      await Promise.resolve()
    }
    expect(session().pendingCancel).toBeNull()
    expect(session().ownerPreserve).toBe(true)
    expect(cancelDestructiveSuppress(session())).toBe(true)
    mockGet.mockReset()

    const settled = detail([
      userTurn("u1"),
      assistantTurn("a1", "post-exhaustion disk"),
    ])
    mockGet.mockResolvedValueOnce(settled)
    actions().refetchDetail(CID)
    await Promise.resolve()
    await Promise.resolve()

    expect(session().localTurns[1]?.blocks[0]).toMatchObject({ text: "local" })
    expect(session().detail?.turns?.[1]?.blocks[0]).not.toMatchObject({
      text: "post-exhaustion disk",
    })
  })

  it("allows destructive commit after manual reload clear", async () => {
    seed({
      detail: detail([userTurn("u0")]),
      localTurns: [userTurn("u1")],
      lastTurnOwned: true,
    })
    startCoordinator()
    mockGet.mockResolvedValueOnce(detail([userTurn("u1")]))
    actions().reloadDetail(CID, { reason: "manual_reload" })
    await Promise.resolve()
    await Promise.resolve()
    expect(session().pendingCancel).toBeNull()

    mockGet.mockResolvedValueOnce(
      detail([userTurn("u1"), assistantTurn("a1", "later")])
    )
    actions().refetchDetail(CID)
    await Promise.resolve()
    await Promise.resolve()
    expect(session().detail?.turns).toHaveLength(2)
  })

  it("allows sync after new prompt clears key", async () => {
    seed({
      detail: detail([userTurn("u0")]),
      localTurns: [
        userTurn("u1"),
        assistantTurn("a1", "x", interruptedOutcome()),
      ],
      lastTurnOwned: true,
    })
    startCoordinator()
    actions().appendOptimisticTurn(CID, userTurn("u2"), "tok")
    expect(session().pendingCancel).toBeNull()

    // awaiting_persist protects owner — drop optimistic and idle to allow
    // a plain refetch path after cancel cleared
    actions().removeOptimisticTurn(CID, "u2")
    // Force idle owner-less for refetch eligibility of destructive path:
    // just call refetchDetail which always runs for owners
    mockGet.mockResolvedValueOnce(
      detail([
        userTurn("u1"),
        assistantTurn("a1", "x", interruptedOutcome()),
        userTurn("u2"),
      ])
    )
    actions().refetchDetail(CID, { preserveLive: true })
    await Promise.resolve()
    await Promise.resolve()
    expect(mockGet).toHaveBeenCalled()
  })

  it("allows destructive sync after remove then re-open session", async () => {
    seed({
      detail: detail([userTurn("u0")]),
      localTurns: [
        userTurn("u1"),
        assistantTurn("a1", "x", interruptedOutcome()),
      ],
      lastTurnOwned: true,
    })
    startCoordinator()
    actions().removeConversation(CID)
    expect(
      useConversationRuntimeStore.getState().byConversationId.get(CID)
    ).toBeUndefined()

    // Recreate session (cold open) and refetch authoritatively
    seed({
      detail: detail([userTurn("u0")]),
      localTurns: [],
      lastTurnOwned: false,
    })
    mockGet.mockResolvedValueOnce(
      detail([userTurn("u1"), assistantTurn("a1", "after remove reopen")])
    )
    actions().refetchDetail(CID)
    await Promise.resolve()
    await Promise.resolve()
    expect(session().detail?.turns[1]?.blocks[0]).toMatchObject({
      text: "after remove reopen",
    })
    expect(session().pendingCancel).toBeNull()
  })

  it("allows destructive sync after external rebind clears key", async () => {
    seed({
      detail: detail([userTurn("u0")]),
      localTurns: [
        userTurn("u1"),
        assistantTurn("a1", "x", interruptedOutcome()),
      ],
      lastTurnOwned: true,
    })
    startCoordinator()
    actions().setExternalId(CID, "sid-rebound")
    expect(session().pendingCancel).toBeNull()

    mockGet.mockResolvedValueOnce(
      detail([userTurn("u1"), assistantTurn("a1", "after rebind")])
    )
    actions().refetchDetail(CID)
    await Promise.resolve()
    await Promise.resolve()
    expect(session().detail?.turns[1]?.blocks[0]).toMatchObject({
      text: "after rebind",
    })
  })
})

// ── Start gates ──

describe("coordinator start gates", () => {
  it("does not start without non-empty provider_turn_id", () => {
    seed({ localTurns: [userTurn("u1")], lastTurnOwned: true })
    actions().startCancelReconcile({
      conversationId: CID,
      connectionId: CONN,
      completionSeq: 1,
      providerTurnId: "",
    })
    expect(session().pendingCancel).toBeNull()
    expect(mockGet).not.toHaveBeenCalled()
  })

  it("does not start without a positive persisted conversation id binding", () => {
    const runtimeId = -3
    useConversationRuntimeStore.setState({
      byConversationId: new Map([
        [
          runtimeId,
          emptySession(runtimeId, {
            dbConversationId: null,
            localTurns: [userTurn("u1")],
            lastTurnOwned: true,
          }),
        ],
      ]),
    })
    actions().startCancelReconcile({
      conversationId: runtimeId,
      connectionId: CONN,
      completionSeq: 1,
      providerTurnId: PROVIDER,
    })
    expect(
      useConversationRuntimeStore.getState().byConversationId.get(runtimeId)
        ?.pendingCancel
    ).toBeNull()
  })

  it("starts when negative runtime key has positive dbConversationId", () => {
    const runtimeId = -3
    useConversationRuntimeStore.setState({
      byConversationId: new Map([
        [
          runtimeId,
          emptySession(runtimeId, {
            dbConversationId: CID,
            localTurns: [userTurn("u1")],
            lastTurnOwned: true,
          }),
        ],
      ]),
    })
    actions().startCancelReconcile({
      conversationId: runtimeId,
      connectionId: CONN,
      completionSeq: 1,
      providerTurnId: PROVIDER,
    })
    expect(
      useConversationRuntimeStore.getState().byConversationId.get(runtimeId)
        ?.pendingCancel
    ).not.toBeNull()
  })
})

// ── RECORD_TURN_OUTCOME attach to current assistant ──

describe("RECORD_TURN_OUTCOME", () => {
  it("attaches outcome to trailing assistant without duplication", () => {
    seed({
      localTurns: [userTurn("u1"), assistantTurn("a1", "body")],
      lastTurnOwned: true,
    })
    recordOutcome()
    expect(session().localTurns).toHaveLength(2)
    expect(session().localTurns[1].outcome).toMatchObject({
      status: "interrupted",
      stop_reason: "cancelled",
      source: "user_stop",
      provider_turn_id: PROVIDER,
    })
  })
})

// ── Task 2: soft fence, owner_preserve, cancelDestructiveSuppress ──

describe("Task2 soft fence + ownerPreserve + cancelDestructiveSuppress", () => {
  it("arms soft fence on Stop with active prompt and blocks destructive refetch", async () => {
    seed({
      detail: detail([userTurn("u0")]),
      optimisticTurns: [userTurn("u1")],
      liveMessage: liveMessage("live-1", "streaming…"),
      syncState: "awaiting_persist",
      activeTurnToken: "tok-1",
      lastTurnOwned: true,
    })
    noteUserStopTurnOwnership(CID)
    expect(session().softFence).toBe(true)
    expect(session().ownerPreserve).toBe(false)
    expect(session().pendingCancel).toBeNull()
    expect(cancelDestructiveSuppress(session())).toBe(true)

    mockGet.mockResolvedValueOnce(
      detail([userTurn("u1"), assistantTurn("a1", "pre-envelope wipe")])
    )
    actions().refetchDetail(CID)
    await Promise.resolve()
    await Promise.resolve()

    expect(session().detail?.turns?.[1]?.blocks[0]).not.toMatchObject({
      text: "pre-envelope wipe",
    })
    // Live/local buffers must not be wiped by unfenced detail.
    expect(session().liveMessage?.content[0]).toMatchObject({
      text: "streaming…",
    })
  })

  it("does not arm soft fence on idle Cancel", () => {
    seed({
      detail: detail([userTurn("u0"), assistantTurn("a0", "done")]),
      localTurns: [],
      optimisticTurns: [],
      liveMessage: null,
      syncState: "idle",
      activeTurnToken: null,
      liveOwnsActiveTurn: false,
      lastTurnOwned: false,
    })
    noteUserStopTurnOwnership(CID)
    expect(session().softFence).toBe(false)
    expect(session().ownerPreserve).toBe(false)
    expect(cancelDestructiveSuppress(session())).toBe(false)
    // Ownership may still be snapshotted for stale-envelope checks.
    expect(__getUserStopOwnershipForTests(CID)).toBeDefined()
  })

  it("does not arm soft fence when idle with liveOwnsActiveTurn retained after COMPLETE_TURN", () => {
    // Delegation-child marker survives promotion/complete; it is not an
    // in-flight prompt signal. Idle Cancel must not arm soft fence solely
    // because liveOwnsActiveTurn is still true.
    seed({
      detail: detail([userTurn("u0")]),
      localTurns: [userTurn("u1"), assistantTurn("a1", "promoted child reply")],
      optimisticTurns: [],
      liveMessage: null,
      syncState: "idle",
      activeTurnToken: null,
      liveOwnsActiveTurn: true,
      lastTurnOwned: true,
    })
    noteUserStopTurnOwnership(CID)
    expect(session().softFence).toBe(false)
    expect(session().ownerPreserve).toBe(false)
    expect(session().pendingCancel).toBeNull()
    expect(cancelDestructiveSuppress(session())).toBe(false)
  })

  it("soft-fence 30s age-out enters ownerPreserve and still suppresses", async () => {
    seed({
      detail: detail([userTurn("u0")]),
      liveMessage: liveMessage("live-1", "partial"),
      syncState: "awaiting_persist",
      activeTurnToken: "tok-1",
      lastTurnOwned: true,
    })
    noteUserStopTurnOwnership(CID)
    expect(session().softFence).toBe(true)

    await vi.advanceTimersByTimeAsync(SOFT_FENCE_AGE_OUT_MS)
    await Promise.resolve()

    expect(session().softFence).toBe(false)
    expect(session().ownerPreserve).toBe(true)
    expect(session().pendingCancel).toBeNull()
    expect(cancelDestructiveSuppress(session())).toBe(true)

    mockGet.mockResolvedValueOnce(
      detail([userTurn("u1"), assistantTurn("a1", "post-ageout wipe")])
    )
    actions().refetchDetail(CID)
    await Promise.resolve()
    await Promise.resolve()
    expect(session().detail?.turns?.[1]?.blocks[0]).not.toMatchObject({
      text: "post-ageout wipe",
    })
  })

  it("user_stop without provider_turn_id records outcome, enters ownerPreserve, no coordinator", async () => {
    seed({
      localTurns: [userTurn("u1"), assistantTurn("a1", "body")],
      liveMessage: liveMessage("live-1", "body"),
      syncState: "awaiting_persist",
      activeTurnToken: "tok-1",
      lastTurnOwned: true,
    })
    noteUserStopTurnOwnership(CID)
    expect(session().softFence).toBe(true)

    // Outcome path for missing provider id (envelope acceptance is Task 4).
    actions().recordTurnOutcome({
      conversationId: CID,
      connectionId: CONN,
      completionSeq: 9,
      outcome: interruptedOutcome(undefined, {
        provider_turn_id: null,
      }),
    })
    enterOwnerPreserve(CID)

    expect(session().localTurns[1].outcome).toMatchObject({
      status: "interrupted",
      source: "user_stop",
    })
    expect(session().localTurns[1].outcome?.provider_turn_id ?? null).toBeNull()
    expect(session().pendingCancel).toBeNull()
    expect(session().softFence).toBe(false)
    expect(session().ownerPreserve).toBe(true)
    expect(cancelDestructiveSuppress(session())).toBe(true)
    expect(mockGet).not.toHaveBeenCalled()

    mockGet.mockResolvedValueOnce(
      detail([userTurn("u1"), assistantTurn("a1", "no-provider wipe")])
    )
    actions().refetchDetail(CID)
    await Promise.resolve()
    await Promise.resolve()
    expect(session().localTurns[1]?.blocks[0]).toMatchObject({ text: "body" })
  })

  it("pendingCancel still suppresses destructive commits (regression)", async () => {
    const local = [
      userTurn("u1"),
      assistantTurn("a1", "keep", interruptedOutcome()),
    ]
    seed({
      detail: detail([userTurn("u0")]),
      localTurns: local,
      lastTurnOwned: true,
    })
    startCoordinator()
    expect(session().pendingCancel).not.toBeNull()
    expect(cancelDestructiveSuppress(session())).toBe(true)

    mockGet.mockResolvedValueOnce(
      detail([userTurn("u1"), assistantTurn("a1", "shorter no fence")])
    )
    actions().refetchDetail(CID)
    await Promise.resolve()
    await Promise.resolve()
    expect(session().localTurns).toEqual(local)
  })

  it("Manual Reload / new prompt / remove restore destructive eligibility", async () => {
    // Manual Reload
    seed({
      detail: detail([userTurn("u0")]),
      liveMessage: liveMessage("live-1", "x"),
      activeTurnToken: "tok-1",
      lastTurnOwned: true,
    })
    noteUserStopTurnOwnership(CID)
    await vi.advanceTimersByTimeAsync(SOFT_FENCE_AGE_OUT_MS)
    expect(session().ownerPreserve).toBe(true)

    mockGet.mockResolvedValueOnce(detail([userTurn("u0")]))
    actions().reloadDetail(CID, { reason: "manual_reload" })
    await Promise.resolve()
    await Promise.resolve()
    expect(session().softFence).toBe(false)
    expect(session().ownerPreserve).toBe(false)
    expect(session().pendingCancel).toBeNull()
    expect(cancelDestructiveSuppress(session())).toBe(false)

    mockGet.mockResolvedValueOnce(
      detail([userTurn("u0"), assistantTurn("a1", "after reload")])
    )
    actions().refetchDetail(CID)
    await Promise.resolve()
    await Promise.resolve()
    expect(session().detail?.turns[1]?.blocks[0]).toMatchObject({
      text: "after reload",
    })

    // New prompt
    seed({
      detail: detail([userTurn("u0")]),
      liveMessage: liveMessage("live-1", "y"),
      activeTurnToken: "tok-2",
      lastTurnOwned: true,
    })
    noteUserStopTurnOwnership(CID)
    expect(session().softFence).toBe(true)
    actions().appendOptimisticTurn(CID, userTurn("u2"), "tok-next")
    expect(session().softFence).toBe(false)
    expect(session().ownerPreserve).toBe(false)
    expect(cancelDestructiveSuppress(session())).toBe(false)

    // Remove
    seed({
      detail: detail([userTurn("u0")]),
      liveMessage: liveMessage("live-1", "z"),
      activeTurnToken: "tok-3",
      lastTurnOwned: true,
    })
    noteUserStopTurnOwnership(CID)
    expect(session().softFence).toBe(true)
    actions().removeConversation(CID)
    expect(
      useConversationRuntimeStore.getState().byConversationId.get(CID)
    ).toBeUndefined()

    seed({
      detail: detail([userTurn("u0")]),
      localTurns: [],
      lastTurnOwned: false,
    })
    expect(cancelDestructiveSuppress(session())).toBe(false)
    mockGet.mockResolvedValueOnce(
      detail([userTurn("u1"), assistantTurn("a1", "after remove")])
    )
    actions().refetchDetail(CID)
    await Promise.resolve()
    await Promise.resolve()
    expect(session().detail?.turns[1]?.blocks[0]).toMatchObject({
      text: "after remove",
    })
  })

  it("retry exhaustion clears pending key but keeps ownerPreserve suppress", async () => {
    const local = [
      userTurn("u1"),
      assistantTurn("a1", "promoted live", interruptedOutcome()),
    ]
    seed({ localTurns: local, lastTurnOwned: true })
    startCoordinator()
    mockGet.mockResolvedValue(
      detail([userTurn("u1"), assistantTurn("a1", "incomplete")])
    )
    for (const delay of CANCEL_RECONCILE_DELAYS_MS) {
      await vi.advanceTimersByTimeAsync(delay)
      await Promise.resolve()
    }

    expect(session().pendingCancel).toBeNull()
    expect(session().ownerPreserve).toBe(true)
    expect(session().softFence).toBe(false)
    expect(cancelDestructiveSuppress(session())).toBe(true)
    expect(session().localTurns).toEqual(local)

    mockGet.mockReset()
    mockGet.mockResolvedValueOnce(
      detail([userTurn("u1"), assistantTurn("a1", "post-exhaustion wipe")])
    )
    actions().refetchDetail(CID)
    await Promise.resolve()
    await Promise.resolve()
    expect(session().localTurns).toEqual(local)
    expect(session().detail?.turns?.[1]?.blocks[0]).not.toMatchObject({
      text: "post-exhaustion wipe",
    })
  })

  it("startCancelReconcile clears softFence while pendingCancel suppresses", () => {
    seed({
      localTurns: [userTurn("u1")],
      liveMessage: liveMessage("live-1", "x"),
      activeTurnToken: "tok-1",
      lastTurnOwned: true,
    })
    noteUserStopTurnOwnership(CID)
    expect(session().softFence).toBe(true)
    startCoordinator()
    expect(session().pendingCancel).not.toBeNull()
    expect(session().softFence).toBe(false)
    expect(cancelDestructiveSuppress(session())).toBe(true)
  })
})
