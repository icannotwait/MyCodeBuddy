/**
 * Task 6 — Completion idempotency around a typed cancellation envelope.
 *
 * A pre-promoted runtime followed by a typed turn_complete (and reverse)
 * records one outcome and starts one coordinator.
 *
 * Envelope path is the sole START_CANCEL_RECONCILE / RECORD_TURN_OUTCOME
 * starter for user_stop; plain COMPLETE_TURN remains promotion-only.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import { readFileSync } from "node:fs"
import { resolve } from "node:path"
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
  enterOwnerPreserve,
  getConversationIdByExternalIdFromStore,
  noteUserStopTurnOwnership,
  resetConversationRuntimeStore,
  resolveRuntimeConversationIdForOwnership,
  SOFT_FENCE_AGE_OUT_MS,
  useConversationRuntimeStore,
  type ConversationRuntimeSession,
} from "@/stores/conversation-runtime-store"
import { acceptUserStopTurnComplete } from "@/contexts/acp-connections-context"

function lastTurn<T>(arr: T[]): T | undefined {
  return arr.length > 0 ? arr[arr.length - 1] : undefined
}

vi.mock("@/lib/api", () => ({
  getFolderConversation: vi.fn(),
}))

const { getFolderConversation } = await import("@/lib/api")
const mockGet = vi.mocked(getFolderConversation)

const CID = 42
const CONN = "conn-user-stop"
const SESSION = "sid-user-stop"
const PROVIDER = "provider-turn-stop-1"
const SEQ = 77

const interruptedOutcome = (
  providerTurnId: string = PROVIDER
): TurnOutcome => ({
  status: "interrupted",
  stop_reason: "cancelled",
  source: "user_stop",
  provider_turn_id: providerTurnId,
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
    externalId: SESSION,
    dbConversationId: conversationId,
    detail: null,
    detailLoading: false,
    detailError: null,
    detailHistoryLoadingOlder: false,
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
    conversationIdByExternalId: new Map([[SESSION, CID]]),
  })
}

const VIRTUAL_ALIAS = -9

function seedAliasPair(): LiveMessage {
  const live = liveMessage("lm-alias", "shared interrupted reply")
  const makeAlias = (conversationId: number) =>
    emptySession(conversationId, {
      externalId: SESSION,
      dbConversationId: CID,
      optimisticTurns: [userTurn(`u-${conversationId}`, "shared prompt")],
      liveMessage: live,
      syncState: "awaiting_persist",
      activeTurnToken: `tok-${conversationId}`,
      lastTurnOwned: true,
    })
  useConversationRuntimeStore.setState({
    byConversationId: new Map([
      [VIRTUAL_ALIAS, makeAlias(VIRTUAL_ALIAS)],
      [CID, makeAlias(CID)],
    ]),
    conversationIdByExternalId: new Map([[SESSION, CID]]),
  })
  noteUserStopTurnOwnership(VIRTUAL_ALIAS)
  noteUserStopTurnOwnership(CID)
  return live
}

function acceptAliasPair(finalLiveMessage: LiveMessage): void {
  acceptUserStopTurnComplete({
    sessionId: SESSION,
    connectionId: CONN,
    completionSeq: SEQ,
    stopReason: "cancelled",
    terminationSource: "user_stop",
    providerTurnId: PROVIDER,
    snapshotConversationId: CID,
    finalLiveMessage,
  })
}

function deferredDetail(): {
  promise: Promise<DbConversationDetail>
  resolve: (detail: DbConversationDetail) => void
} {
  let resolve!: (detail: DbConversationDetail) => void
  const promise = new Promise<DbConversationDetail>((done) => {
    resolve = done
  })
  return { promise, resolve }
}

function session(): ConversationRuntimeSession {
  const s = useConversationRuntimeStore.getState().byConversationId.get(CID)
  if (!s) throw new Error("missing session")
  return s
}

function actions() {
  return useConversationRuntimeStore.getState().actions
}

/** Plain COMPLETE_TURN promotion without cancellation envelope metadata. */
function promoteBeforeEnvelope(live?: LiveMessage | null): void {
  actions().completeTurn(CID, live === undefined ? undefined : live)
}

function envelopeUserStop(
  opts: {
    seq?: number
    providerTurnId?: string | null
    stopReason?: string
    terminationSource?: "user_stop" | null
  } = {}
): void {
  acceptUserStopTurnComplete({
    sessionId: SESSION,
    connectionId: CONN,
    completionSeq: opts.seq ?? SEQ,
    stopReason: opts.stopReason ?? "cancelled",
    terminationSource:
      opts.terminationSource === undefined
        ? "user_stop"
        : opts.terminationSource,
    providerTurnId:
      opts.providerTurnId === undefined ? PROVIDER : opts.providerTurnId,
    snapshotConversationId: CID,
  })
}

function detailWithFence(): DbConversationDetail {
  return {
    summary: {
      id: CID,
      folder_id: 1,
      agent_type: "codex",
      title: "t",
      title_locked: false,
      auto_title_finalized: false,
      status: "cancelled",
      awaiting_reply_token: null,
      kind: "regular",
      model: null,
      git_branch: null,
      external_id: SESSION,
      message_count: 2,
      child_count: 0,
      created_at: "2026-07-25T00:00:00.000Z",
      updated_at: "2026-07-25T00:00:02.000Z",
      pinned_at: null,
    },
    turns: [
      userTurn("u1"),
      assistantTurn("a-persisted", "full persisted", interruptedOutcome()),
    ],
    session_stats: null,
  }
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

// ── FE case 11: completion orderings ──

describe("FE11 completion orderings", () => {
  it("pre-promotion then late typed turn_complete: one outcome, one coordinator", async () => {
    seed({
      localTurns: [userTurn("u1")],
      optimisticTurns: [],
      liveMessage: liveMessage("lm1", "partial live"),
      syncState: "awaiting_persist",
      activeTurnToken: "tok-1",
      lastTurnOwned: true,
    })
    mockGet.mockResolvedValue(detailWithFence())

    // Cancel ownership snapshotted at user Stop (before turn_complete).
    noteUserStopTurnOwnership(CID)

    // Path A: plain COMPLETE_TURN promotes first (no envelope fields).
    promoteBeforeEnvelope()
    expect(session().liveMessage).toBeNull()
    expect(session().localTurns.some((t) => t.role === "assistant")).toBe(true)
    expect(lastTurn(session().localTurns)?.outcome).toBeUndefined()
    expect(session().pendingCancel).toBeNull()
    expect(mockGet).not.toHaveBeenCalled()

    // Path B: late typed envelope records outcome + starts coordinator once.
    envelopeUserStop()
    expect(
      session().localTurns.filter((t) => t.role === "assistant")
    ).toHaveLength(1)
    expect(lastTurn(session().localTurns)?.outcome).toMatchObject({
      status: "interrupted",
      stop_reason: "cancelled",
      source: "user_stop",
      provider_turn_id: PROVIDER,
    })
    expect(session().pendingCancel).toMatchObject({
      connectionId: CONN,
      completionSeq: SEQ,
      providerTurnId: PROVIDER,
    })

    // Duplicate envelope delivery: still one outcome, same key.
    envelopeUserStop()
    expect(
      session().localTurns.filter((t) => t.role === "assistant")
    ).toHaveLength(1)
    expect(session().pendingCancel?.completionSeq).toBe(SEQ)

    await vi.advanceTimersByTimeAsync(100)
    expect(mockGet).toHaveBeenCalledTimes(1)
  })

  it("typed envelope then duplicate promotion: one outcome, one coordinator, content kept", async () => {
    seed({
      localTurns: [userTurn("u1")],
      optimisticTurns: [],
      liveMessage: liveMessage("lm1", "partial live before promote"),
      syncState: "awaiting_persist",
      activeTurnToken: "tok-2",
      lastTurnOwned: true,
    })
    mockGet.mockResolvedValue(detailWithFence())
    noteUserStopTurnOwnership(CID)

    // Path B first: envelope owns outcome + coordinator (and may promote).
    envelopeUserStop()
    expect(session().pendingCancel).toMatchObject({
      connectionId: CONN,
      completionSeq: SEQ,
      providerTurnId: PROVIDER,
    })
    const assistantsAfterEnvelope = session().localTurns.filter(
      (t) => t.role === "assistant"
    )
    expect(assistantsAfterEnvelope).toHaveLength(1)
    expect(assistantsAfterEnvelope[0].outcome).toMatchObject({
      source: "user_stop",
      provider_turn_id: PROVIDER,
    })
    // Content from live buffer must be on the promoted assistant (not empty
    // outcome-only shell + separate content turn).
    const textBlocks = assistantsAfterEnvelope[0].blocks.filter(
      (b) => b.type === "text"
    )
    expect(
      textBlocks.some((b) => b.type === "text" && b.text.includes("partial"))
    ).toBe(true)

    // Path A late: plain COMPLETE_TURN is promotion-only (already drained — no-op).
    promoteBeforeEnvelope()
    expect(
      session().localTurns.filter((t) => t.role === "assistant")
    ).toHaveLength(1)
    expect(session().pendingCancel?.completionSeq).toBe(SEQ)
    expect(mockGet).not.toHaveBeenCalled()

    await vi.advanceTimersByTimeAsync(100)
    expect(mockGet).toHaveBeenCalledTimes(1)
  })

  it("late cancel envelope while next prompt B is active does not promote or stamp B", async () => {
    seed({
      localTurns: [userTurn("u1")],
      optimisticTurns: [],
      liveMessage: liveMessage("lm1", "cancel live A"),
      syncState: "awaiting_persist",
      activeTurnToken: "tok-A",
      lastTurnOwned: true,
    })
    mockGet.mockResolvedValue(detailWithFence())

    // User Stop on turn A — ownership fenced to cancelGeneration + tok-A.
    noteUserStopTurnOwnership(CID)
    const ownedGen = __getUserStopOwnershipForTests(CID)?.cancelGeneration
    expect(ownedGen).toBeTypeOf("number")
    promoteBeforeEnvelope()
    const afterA = session()
    expect(afterA.activeTurnToken).toBeNull()
    expect(afterA.localTurns.some((t) => t.role === "assistant")).toBe(true)
    const assistantAfterA = lastTurn(
      afterA.localTurns.filter((t) => t.role === "assistant")
    )
    expect(assistantAfterA?.outcome).toBeUndefined()

    // Immediate / queued next prompt B bumps cancelGeneration + replaces token.
    actions().appendOptimisticTurn(
      CID,
      userTurn("u2", "next prompt B"),
      "tok-B"
    )
    expect(session().activeTurnToken).toBe("tok-B")
    expect(session().optimisticTurns).toHaveLength(1)

    // Late typed envelope for cancelled A must not promote B or attach to B.
    envelopeUserStop()
    expect(session().pendingCancel).toBeNull()
    expect(session().activeTurnToken).toBe("tok-B")
    expect(session().optimisticTurns).toHaveLength(1)
    expect(session().optimisticTurns[0]?.id).toBe("u2")
    // Cancelled assistant stays without being wiped; no outcome stamped on B.
    const assistants = session().localTurns.filter(
      (t) => t.role === "assistant"
    )
    expect(assistants).toHaveLength(1)
    expect(assistants[0]?.outcome).toBeUndefined()
    expect(mockGet).not.toHaveBeenCalled()
  })

  it("late cancel envelope after next turn B completed still rejects (monotonic gen)", async () => {
    seed({
      localTurns: [userTurn("u1")],
      optimisticTurns: [],
      liveMessage: liveMessage("lm1", "cancel live A"),
      syncState: "awaiting_persist",
      activeTurnToken: "tok-A",
      lastTurnOwned: true,
    })
    mockGet.mockResolvedValue(detailWithFence())

    noteUserStopTurnOwnership(CID)
    promoteBeforeEnvelope()

    // Next prompt B, then B completes (clears activeTurnToken to null again).
    actions().appendOptimisticTurn(
      CID,
      userTurn("u2", "next prompt B"),
      "tok-B"
    )
    actions().setLiveMessage(CID, liveMessage("lm-b", "reply B"))
    promoteBeforeEnvelope()
    expect(session().activeTurnToken).toBeNull()
    expect(session().liveMessage).toBeNull()
    expect(session().optimisticTurns).toHaveLength(0)
    const assistantsAfterB = session().localTurns.filter(
      (t) => t.role === "assistant"
    )
    expect(assistantsAfterB.length).toBeGreaterThanOrEqual(2)
    const bAssistant = lastTurn(assistantsAfterB)
    expect(bAssistant?.outcome).toBeUndefined()
    // Trailing local turn is B's assistant — token-only fence would miss this.
    expect(lastTurn(session().localTurns)?.role).toBe("assistant")

    // Late typed envelope for A must not stamp B or start A's coordinator.
    envelopeUserStop()
    expect(session().pendingCancel).toBeNull()
    expect(lastTurn(session().localTurns)?.outcome).toBeUndefined()
    for (const a of session().localTurns.filter(
      (t) => t.role === "assistant"
    )) {
      expect(a.outcome).toBeUndefined()
    }
    expect(mockGet).not.toHaveBeenCalled()
  })

  it("noteUserStopTurnOwnership keys by runtime id when only positive DB id is passed", () => {
    const RUNTIME = -9001
    const DB = 4242
    useConversationRuntimeStore.setState({
      byConversationId: new Map([
        [
          RUNTIME,
          emptySession(RUNTIME, {
            dbConversationId: DB,
            externalId: SESSION,
            activeTurnToken: "tok-draft",
            syncState: "awaiting_persist",
            liveMessage: liveMessage("lm", "draft live"),
          }),
        ],
      ]),
      conversationIdByExternalId: new Map([[SESSION, RUNTIME]]),
    })

    // Prefer external-id → runtime key (cancel path); also accept positive DB
    // id that only exists as dbConversationId on the virtual session.
    expect(resolveRuntimeConversationIdForOwnership(DB)).toBe(RUNTIME)
    expect(getConversationIdByExternalIdFromStore(SESSION)).toBe(RUNTIME)

    noteUserStopTurnOwnership(DB)
    expect(__getUserStopOwnershipForTests(RUNTIME)).toMatchObject({
      activeTurnToken: "tok-draft",
      cancelGeneration: 0,
    })
    // Ownership is on the runtime key, not the positive DB id alone.
    expect(__getUserStopOwnershipForTests(DB)?.activeTurnToken).toBe(
      "tok-draft"
    )

    // Next prompt on runtime key advances gen; late accept via runtime id is stale.
    actions().appendOptimisticTurn(
      RUNTIME,
      userTurn("u-next", "after draft stop"),
      "tok-next"
    )
    acceptUserStopTurnComplete({
      sessionId: SESSION,
      connectionId: CONN,
      completionSeq: SEQ,
      stopReason: "cancelled",
      terminationSource: "user_stop",
      providerTurnId: PROVIDER,
      snapshotConversationId: RUNTIME,
    })
    expect(
      useConversationRuntimeStore.getState().byConversationId.get(RUNTIME)
        ?.pendingCancel
    ).toBeNull()
  })

  it("ordinary end_turn does not record cancel outcome or start coordinator", () => {
    seed({
      localTurns: [userTurn("u1")],
      liveMessage: liveMessage("lm1", "natural end"),
      syncState: "awaiting_persist",
      activeTurnToken: "tok-3",
    })
    promoteBeforeEnvelope()
    acceptUserStopTurnComplete({
      sessionId: SESSION,
      connectionId: CONN,
      completionSeq: SEQ,
      stopReason: "end_turn",
      terminationSource: null,
      providerTurnId: null,
      snapshotConversationId: CID,
    })
    expect(lastTurn(session().localTurns)?.outcome).toBeUndefined()
    expect(session().pendingCancel).toBeNull()
    expect(mockGet).not.toHaveBeenCalled()
  })

  it("user_stop without provider_turn_id records outcome, enters ownerPreserve, no coordinator", () => {
    seed({
      localTurns: [userTurn("u1")],
      liveMessage: liveMessage("lm1", "live"),
      syncState: "awaiting_persist",
      activeTurnToken: "tok-4",
    })
    noteUserStopTurnOwnership(CID)
    envelopeUserStop({ providerTurnId: null })
    expect(lastTurn(session().localTurns)?.outcome).toMatchObject({
      status: "interrupted",
      stop_reason: "cancelled",
      source: "user_stop",
    })
    expect(session().pendingCancel).toBeNull()
    expect(session().softFence).toBe(false)
    expect(session().ownerPreserve).toBe(true)
    expect(mockGet).not.toHaveBeenCalled()
  })

  it("unbound detail id (<=0) records outcome, no coordinator, enters ownerPreserve", () => {
    const RUNTIME = -9100
    useConversationRuntimeStore.setState({
      byConversationId: new Map([
        [
          RUNTIME,
          emptySession(RUNTIME, {
            dbConversationId: null,
            externalId: SESSION,
            localTurns: [userTurn("u1")],
            liveMessage: liveMessage("lm1", "draft live"),
            syncState: "awaiting_persist",
            activeTurnToken: "tok-unbound",
            lastTurnOwned: true,
          }),
        ],
      ]),
      conversationIdByExternalId: new Map([[SESSION, RUNTIME]]),
    })
    noteUserStopTurnOwnership(RUNTIME)
    acceptUserStopTurnComplete({
      sessionId: SESSION,
      connectionId: CONN,
      completionSeq: SEQ,
      stopReason: "cancelled",
      terminationSource: "user_stop",
      providerTurnId: PROVIDER,
      snapshotConversationId: RUNTIME,
    })
    const s = useConversationRuntimeStore
      .getState()
      .byConversationId.get(RUNTIME)!
    expect(lastTurn(s.localTurns)?.outcome).toMatchObject({
      status: "interrupted",
      source: "user_stop",
      provider_turn_id: PROVIDER,
    })
    expect(s.pendingCancel).toBeNull()
    expect(s.softFence).toBe(false)
    expect(s.ownerPreserve).toBe(true)
    expect(mockGet).not.toHaveBeenCalled()
  })

  it("unbound accept then migrate to positive id: redelivered envelope never starts coordinator", () => {
    // Important fix: first unbound acceptance is terminal for coordinator start.
    const RUNTIME = -9101
    const TO = 9101
    useConversationRuntimeStore.setState({
      byConversationId: new Map([
        [
          RUNTIME,
          emptySession(RUNTIME, {
            dbConversationId: null,
            externalId: SESSION,
            localTurns: [userTurn("u1")],
            liveMessage: liveMessage("lm1", "draft live"),
            syncState: "awaiting_persist",
            activeTurnToken: "tok-unbound-mig",
            lastTurnOwned: true,
          }),
        ],
      ]),
      conversationIdByExternalId: new Map([[SESSION, RUNTIME]]),
    })
    noteUserStopTurnOwnership(RUNTIME)
    acceptUserStopTurnComplete({
      sessionId: SESSION,
      connectionId: CONN,
      completionSeq: SEQ,
      stopReason: "cancelled",
      terminationSource: "user_stop",
      providerTurnId: PROVIDER,
      snapshotConversationId: RUNTIME,
    })
    const unbound = useConversationRuntimeStore
      .getState()
      .byConversationId.get(RUNTIME)!
    expect(unbound.pendingCancel).toBeNull()
    expect(unbound.ownerPreserve).toBe(true)
    expect(mockGet).not.toHaveBeenCalled()

    // Runtime-key migrate to positive id (DB bind path).
    actions().migrateConversation(RUNTIME, TO)
    // Positive db binding on destination.
    actions().setDbConversationId(TO, TO)

    const afterMigrate = useConversationRuntimeStore
      .getState()
      .byConversationId.get(TO)!
    expect(afterMigrate.ownerPreserve).toBe(true)
    expect(afterMigrate.pendingCancel).toBeNull()

    // Redeliver same completion identity after positive bind.
    mockGet.mockResolvedValue(detailWithFence())
    acceptUserStopTurnComplete({
      sessionId: SESSION,
      connectionId: CONN,
      completionSeq: SEQ,
      stopReason: "cancelled",
      terminationSource: "user_stop",
      providerTurnId: PROVIDER,
      snapshotConversationId: TO,
    })
    const redelivered = useConversationRuntimeStore
      .getState()
      .byConversationId.get(TO)!
    expect(redelivered.pendingCancel).toBeNull()
    expect(redelivered.ownerPreserve).toBe(true)
    expect(mockGet).not.toHaveBeenCalled()
    // Footer not duplicated.
    expect(
      redelivered.localTurns.filter((t) => t.role === "assistant")
    ).toHaveLength(1)
  })

  it("late envelope after soft-fence age-out still current may start coordinator", async () => {
    seed({
      localTurns: [userTurn("u1")],
      liveMessage: liveMessage("lm1", "partial"),
      syncState: "awaiting_persist",
      activeTurnToken: "tok-age",
      lastTurnOwned: true,
      dbConversationId: CID,
    })
    mockGet.mockResolvedValue(detailWithFence())
    noteUserStopTurnOwnership(CID)
    expect(session().softFence).toBe(true)

    await vi.advanceTimersByTimeAsync(SOFT_FENCE_AGE_OUT_MS)
    expect(session().softFence).toBe(false)
    expect(session().ownerPreserve).toBe(true)

    envelopeUserStop()
    expect(session().pendingCancel).toMatchObject({
      connectionId: CONN,
      completionSeq: SEQ,
      providerTurnId: PROVIDER,
    })
    await vi.advanceTimersByTimeAsync(100)
    expect(mockGet).toHaveBeenCalledTimes(1)
  })

  it("late envelope after age-out + next prompt is stale and no-ops", async () => {
    seed({
      localTurns: [userTurn("u1")],
      liveMessage: liveMessage("lm1", "partial"),
      syncState: "awaiting_persist",
      activeTurnToken: "tok-age-stale",
      lastTurnOwned: true,
      dbConversationId: CID,
    })
    mockGet.mockResolvedValue(detailWithFence())
    noteUserStopTurnOwnership(CID)
    await vi.advanceTimersByTimeAsync(SOFT_FENCE_AGE_OUT_MS)
    expect(session().ownerPreserve).toBe(true)

    actions().appendOptimisticTurn(
      CID,
      userTurn("u2", "next prompt B"),
      "tok-B"
    )
    envelopeUserStop()
    expect(session().pendingCancel).toBeNull()
    expect(lastTurn(session().localTurns)?.outcome).toBeUndefined()
    expect(mockGet).not.toHaveBeenCalled()
  })

  it("runtime-key migrate keeps late envelope current (no gen bump)", () => {
    const FROM = CID
    const TO = 8801
    seed({
      localTurns: [userTurn("u1")],
      liveMessage: liveMessage("lm1", "partial"),
      syncState: "awaiting_persist",
      activeTurnToken: "tok-mig",
      lastTurnOwned: true,
      externalId: SESSION,
      dbConversationId: FROM,
    })
    mockGet.mockResolvedValue(detailWithFence())
    noteUserStopTurnOwnership(FROM)
    actions().migrateConversation(FROM, TO)

    // Envelope resolves via external id → TO; ownership/gen migrated without bump.
    acceptUserStopTurnComplete({
      sessionId: SESSION,
      connectionId: CONN,
      completionSeq: SEQ,
      stopReason: "cancelled",
      terminationSource: "user_stop",
      providerTurnId: PROVIDER,
      snapshotConversationId: FROM,
    })
    const toSession = useConversationRuntimeStore
      .getState()
      .byConversationId.get(TO)!
    expect(toSession.pendingCancel).toMatchObject({
      conversationId: TO,
      completionSeq: SEQ,
      providerTurnId: PROVIDER,
    })
    expect(
      toSession.localTurns.filter((t) => t.role === "assistant")
    ).toHaveLength(1)
    expect(lastTurn(toSession.localTurns)?.outcome).toMatchObject({
      source: "user_stop",
    })

    // Duplicate envelope does not second footer / second coordinator key.
    acceptUserStopTurnComplete({
      sessionId: SESSION,
      connectionId: CONN,
      completionSeq: SEQ,
      stopReason: "cancelled",
      terminationSource: "user_stop",
      providerTurnId: PROVIDER,
      snapshotConversationId: TO,
    })
    expect(
      toSession.localTurns.filter((t) => t.role === "assistant")
    ).toHaveLength(1)
    expect(
      useConversationRuntimeStore.getState().byConversationId.get(TO)
        ?.pendingCancel?.completionSeq
    ).toBe(SEQ)
  })

  it("elects the durable alias once and reconciles every unchanged follower", async () => {
    const finalLiveMessage = seedAliasPair()
    mockGet.mockResolvedValue(detailWithFence())

    acceptAliasPair(finalLiveMessage)

    for (const conversationId of [VIRTUAL_ALIAS, CID]) {
      const alias = useConversationRuntimeStore
        .getState()
        .byConversationId.get(conversationId)!
      expect(alias.localTurns.at(-1)).toMatchObject({
        role: "assistant",
        blocks: [{ type: "text", text: "shared interrupted reply" }],
        outcome: { source: "user_stop", provider_turn_id: PROVIDER },
      })
    }
    expect(
      useConversationRuntimeStore.getState().byConversationId.get(CID)
        ?.pendingCancel
    ).toMatchObject({ completionSeq: SEQ })
    expect(
      useConversationRuntimeStore.getState().byConversationId.get(VIRTUAL_ALIAS)
        ?.ownerPreserve
    ).toBe(true)

    await vi.advanceTimersByTimeAsync(CANCEL_RECONCILE_DELAYS_MS[0])
    expect(mockGet).toHaveBeenCalledTimes(1)
    expect(mockGet).toHaveBeenCalledWith(42, expect.any(Object))
    for (const conversationId of [VIRTUAL_ALIAS, CID]) {
      expect(
        useConversationRuntimeStore
          .getState()
          .byConversationId.get(conversationId)
          ?.detail?.turns.at(-1)
      ).toMatchObject({ id: "a-persisted" })
    }
    expect(getConversationIdByExternalIdFromStore(SESSION)).toBe(CID)
  })

  it("prefers the positive durable alias when no snapshot owner is available", () => {
    const finalLiveMessage = seedAliasPair()

    acceptUserStopTurnComplete({
      sessionId: SESSION,
      connectionId: CONN,
      completionSeq: SEQ,
      stopReason: "cancelled",
      terminationSource: "user_stop",
      providerTurnId: PROVIDER,
      finalLiveMessage,
    })

    expect(
      useConversationRuntimeStore.getState().byConversationId.get(CID)
        ?.pendingCancel
    ).toMatchObject({ completionSeq: SEQ })
    expect(
      useConversationRuntimeStore.getState().byConversationId.get(VIRTUAL_ALIAS)
        ?.pendingCancel
    ).toBeNull()
  })

  it("transfers an invalidated coordinator to an unchanged durable follower", async () => {
    seedAliasPair()
    const runtimeActions = actions()
    const outcome = {
      status: "interrupted" as const,
      stop_reason: "cancelled",
      source: "user_stop" as const,
      provider_turn_id: PROVIDER,
    }
    for (const conversationId of [VIRTUAL_ALIAS, CID]) {
      runtimeActions.recordTurnOutcome({
        conversationId,
        connectionId: CONN,
        completionSeq: SEQ,
        outcome,
      })
    }
    enterOwnerPreserve(CID)
    runtimeActions.startCancelReconcile({
      conversationId: VIRTUAL_ALIAS,
      connectionId: CONN,
      completionSeq: SEQ,
      providerTurnId: PROVIDER,
      sessionId: SESSION,
      followerConversationIds: [CID],
    })
    mockGet.mockResolvedValue(detailWithFence())

    runtimeActions.removeConversation(VIRTUAL_ALIAS)

    expect(
      useConversationRuntimeStore.getState().byConversationId.get(CID)
        ?.pendingCancel
    ).toMatchObject({ completionSeq: SEQ })
    await vi.advanceTimersByTimeAsync(CANCEL_RECONCILE_DELAYS_MS[0])
    expect(mockGet).toHaveBeenCalledTimes(1)
    expect(mockGet).toHaveBeenCalledWith(CID, expect.any(Object))
    expect(
      useConversationRuntimeStore
        .getState()
        .byConversationId.get(CID)
        ?.detail?.turns.at(-1)
    ).toMatchObject({ id: "a-persisted" })
  })

  it("keeps the durable external-id owner when a virtual alias is removed", () => {
    seedAliasPair()

    actions().removeConversation(VIRTUAL_ALIAS)

    expect(getConversationIdByExternalIdFromStore(SESSION)).toBe(CID)
  })

  it("re-elects a remaining alias when the indexed external-id owner is removed", () => {
    seedAliasPair()

    actions().removeConversation(CID)

    expect(getConversationIdByExternalIdFromStore(SESSION)).toBe(VIRTUAL_ALIAS)
  })

  it("does not retain a migrated follower as the coordinator owner follower", async () => {
    const finalLiveMessage = seedAliasPair()
    mockGet.mockResolvedValue(detailWithFence())
    acceptAliasPair(finalLiveMessage)

    actions().migrateConversation(VIRTUAL_ALIAS, CID)
    const generationBefore = __getCancelGenerationForTests(CID)

    await vi.advanceTimersByTimeAsync(CANCEL_RECONCILE_DELAYS_MS[0])

    expect(mockGet).toHaveBeenCalledTimes(1)
    expect(
      useConversationRuntimeStore
        .getState()
        .byConversationId.get(CID)
        ?.detail?.turns.at(-1)
    ).toMatchObject({ id: "a-persisted" })
    expect(__getCancelGenerationForTests(CID)).toBe(generationBefore + 1)
  })

  it("does not fan reconciliation into a follower that started a new turn", async () => {
    const finalLiveMessage = seedAliasPair()
    const pending = deferredDetail()
    mockGet.mockReturnValue(pending.promise)

    acceptAliasPair(finalLiveMessage)
    await vi.advanceTimersByTimeAsync(CANCEL_RECONCILE_DELAYS_MS[0])
    expect(mockGet).toHaveBeenCalledTimes(1)

    actions().appendOptimisticTurn(
      VIRTUAL_ALIAS,
      userTurn("u-next", "new follower prompt"),
      "tok-next"
    )
    pending.resolve(detailWithFence())
    await Promise.resolve()
    await Promise.resolve()

    const owner = useConversationRuntimeStore
      .getState()
      .byConversationId.get(CID)!
    const follower = useConversationRuntimeStore
      .getState()
      .byConversationId.get(VIRTUAL_ALIAS)!
    expect(owner.detail?.turns.at(-1)).toMatchObject({ id: "a-persisted" })
    expect(follower.detail).toBeNull()
    expect(follower.optimisticTurns.map((turn) => turn.id)).toContain("u-next")
    expect(follower.activeTurnToken).toBe("tok-next")
  })
})

// ── Wiring audits (source-level; sole starter + Manual Reload) ──

describe("dual-path wiring audits", () => {
  const root = resolve(__dirname, "../..")

  it("acp-connections-context is the sole user_stop starter (calls store APIs)", () => {
    const src = readFileSync(
      resolve(root, "src/contexts/acp-connections-context.tsx"),
      "utf8"
    )
    expect(src).toContain("acceptUserStopTurnComplete")
    expect(src).toContain('termination_source === "user_stop"')
    expect(src).toContain("recordTurnOutcome")
    expect(src).toContain("startCancelReconcile")
    expect(src).toContain("noteUserStopTurnOwnership")
    expect(src).toContain("isStaleUserStopEnvelope")
    // Cancel path prefers external-id runtime key before conn.conversationId.
    expect(src).toMatch(
      /getConversationIdByExternalIdFromStore\(conn\.sessionId\)[\s\S]*?conn\.conversationId/
    )
  })

  it("conversation-session-surface leaves cancel reconciliation to the provider", () => {
    const src = readFileSync(
      resolve(
        root,
        "src/components/conversations/conversation-session-surface.tsx"
      ),
      "utf8"
    )
    expect(src).toContain("reloadDetail")
    expect(src).toContain('reason: "manual_reload"')
    expect(src).not.toContain("startCancelReconcile")
    expect(src).not.toContain("recordTurnOutcome")
  })
})
