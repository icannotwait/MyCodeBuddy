/**
 * Task 6 — Dual-path completion wiring.
 *
 * Design FE case 11: status-edge promotion then late typed turn_complete
 * (and reverse) records one outcome and starts one coordinator.
 *
 * Envelope path is the sole START_CANCEL_RECONCILE / RECORD_TURN_OUTCOME
 * starter for user_stop; status-edge remains promotion-only.
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
  __getUserStopOwnershipForTests,
  getConversationIdByExternalIdFromStore,
  noteUserStopTurnOwnership,
  resetConversationRuntimeStore,
  resolveRuntimeConversationIdForOwnership,
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
    ...overrides,
  }
}

function seed(overrides: Partial<ConversationRuntimeSession> = {}): void {
  useConversationRuntimeStore.setState({
    byConversationId: new Map([[CID, emptySession(CID, overrides)]]),
    conversationIdByExternalId: new Map([[SESSION, CID]]),
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

/** Status-edge / COMPLETE_TURN promotion path (session-surface). */
function promoteStatusEdge(live?: LiveMessage | null): void {
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

// ── FE case 11: dual-path orderings ──

describe("FE11 dual-path completion orderings", () => {
  it("status-edge then late typed turn_complete: one outcome, one coordinator", async () => {
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

    // Path A: status-edge promotes first (no envelope fields).
    promoteStatusEdge()
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

  it("typed envelope then status-edge: one outcome, one coordinator, content kept", async () => {
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

    // Path A late: status-edge is promotion-only (already drained — no-op).
    promoteStatusEdge()
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
    promoteStatusEdge()
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
    promoteStatusEdge()

    // Next prompt B, then B completes (clears activeTurnToken to null again).
    actions().appendOptimisticTurn(
      CID,
      userTurn("u2", "next prompt B"),
      "tok-B"
    )
    actions().setLiveMessage(CID, liveMessage("lm-b", "reply B"))
    promoteStatusEdge()
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
    promoteStatusEdge()
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

  it("user_stop without provider_turn_id records outcome but does not start coordinator", () => {
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
    expect(mockGet).not.toHaveBeenCalled()
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

  it("conversation-session-surface promotes only and Manual Reload uses reloadDetail", () => {
    const src = readFileSync(
      resolve(
        root,
        "src/components/conversations/conversation-session-surface.tsx"
      ),
      "utf8"
    )
    expect(src).toContain("completeLiveTranscriptTurn")
    expect(src).toContain("reloadDetail")
    expect(src).toContain('reason: "manual_reload"')
    expect(src).not.toContain("startCancelReconcile")
    expect(src).not.toContain("recordTurnOutcome")
  })

  it("conversation-detail-panel background listener does not double-start coordinator", () => {
    const src = readFileSync(
      resolve(
        root,
        "src/components/conversations/conversation-detail-panel.tsx"
      ),
      "utf8"
    )
    expect(src).toContain("completeLiveTranscriptTurn")
    expect(src).not.toContain("startCancelReconcile")
    expect(src).not.toContain("recordTurnOutcome")
    expect(src).not.toContain("acceptUserStopTurnComplete")
  })
})
