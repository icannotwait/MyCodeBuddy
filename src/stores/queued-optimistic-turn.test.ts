import { afterEach, describe, expect, it, vi } from "vitest"
import type { DbConversationDetail, MessageTurn } from "@/lib/types"
import {
  resetConversationRuntimeStore,
  useConversationRuntimeStore,
} from "@/stores/conversation-runtime-store"

vi.mock("@/lib/api", () => ({
  getFolderConversation: vi.fn(),
  getFolderConversationTurns: vi.fn(),
  saveTurnGenerationStat: vi.fn(),
}))

const { getFolderConversation } = await import("@/lib/api")
const mockGet = vi.mocked(getFolderConversation)

const CID = 42

function userTurn(id: string, text = id): MessageTurn {
  return {
    id,
    role: "user",
    blocks: [{ type: "text", text }],
    timestamp: "2026-05-28T00:00:00.000Z",
  }
}

afterEach(() => {
  resetConversationRuntimeStore()
})

describe("queued optimistic user turns", () => {
  it("queuePending appends a visible turn without awaiting_persist", () => {
    const actions = useConversationRuntimeStore.getState().actions
    actions.appendOptimisticTurn(CID, userTurn("q1", "follow-up"), "q1", {
      queuePending: true,
    })
    const session = useConversationRuntimeStore
      .getState()
      .byConversationId.get(CID)
    expect(session?.optimisticTurns.map((t) => t.id)).toEqual(["q1"])
    expect(session?.syncState).toBe("idle")
    expect(session?.activeTurnToken).toBeNull()
  })

  it("arming a queued turn for dispatch does not duplicate it", () => {
    const actions = useConversationRuntimeStore.getState().actions
    actions.appendOptimisticTurn(CID, userTurn("q1", "follow-up"), "q1", {
      queuePending: true,
    })
    actions.appendOptimisticTurn(CID, userTurn("q1", "follow-up"), "q1")
    const session = useConversationRuntimeStore
      .getState()
      .byConversationId.get(CID)
    expect(session?.optimisticTurns).toHaveLength(1)
    expect(session?.syncState).toBe("awaiting_persist")
    expect(session?.activeTurnToken).toBe("q1")
  })

  it("completeTurn promotes the in-flight turn and keeps queued follow-ups", () => {
    const actions = useConversationRuntimeStore.getState().actions
    actions.appendOptimisticTurn(
      CID,
      userTurn("in-flight", "first"),
      "in-flight"
    )
    actions.appendOptimisticTurn(CID, userTurn("queued", "second"), "queued", {
      queuePending: true,
    })
    actions.completeTurn(CID, {
      id: "live-1",
      role: "assistant",
      content: [{ type: "text", text: "done" }],
      startedAt: 1,
    })
    const session = useConversationRuntimeStore
      .getState()
      .byConversationId.get(CID)
    expect(session?.localTurns.map((t) => t.id)).toEqual(
      expect.arrayContaining(["in-flight"])
    )
    expect(session?.localTurns.map((t) => t.id)).not.toContain("queued")
    expect(session?.optimisticTurns.map((t) => t.id)).toEqual(["queued"])
    expect(session?.syncState).toBe("idle")
  })

  it("parking a bounced in-flight turn keeps the bubble and unblocks flush", () => {
    const actions = useConversationRuntimeStore.getState().actions
    actions.appendOptimisticTurn(CID, userTurn("b", "busy"), "b")
    expect(
      useConversationRuntimeStore.getState().byConversationId.get(CID)
        ?.syncState
    ).toBe("awaiting_persist")
    actions.appendOptimisticTurn(CID, userTurn("b", "busy"), "b", {
      queuePending: true,
    })
    const session = useConversationRuntimeStore
      .getState()
      .byConversationId.get(CID)
    expect(session?.optimisticTurns.map((t) => t.id)).toEqual(["b"])
    expect(session?.syncState).toBe("idle")
    expect(session?.activeTurnToken).toBeNull()
  })

  it("settled detail refetch keeps queued follow-up bubbles", async () => {
    const actions = useConversationRuntimeStore.getState().actions
    actions.appendOptimisticTurn(CID, userTurn("q1", "follow-up"), "q1", {
      queuePending: true,
    })
    const emptyDetail: DbConversationDetail = {
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
        message_count: 0,
        child_count: 0,
        created_at: "2026-05-28T00:00:00.000Z",
        updated_at: "2026-05-28T00:00:00.000Z",
        pinned_at: null,
      },
      turns: [],
      session_stats: null,
    }
    mockGet.mockResolvedValue(emptyDetail)
    actions.refetchDetail(CID)
    await Promise.resolve()
    await Promise.resolve()
    const session = useConversationRuntimeStore
      .getState()
      .byConversationId.get(CID)
    expect(session?.optimisticTurns.map((t) => t.id)).toEqual(["q1"])
  })
})
