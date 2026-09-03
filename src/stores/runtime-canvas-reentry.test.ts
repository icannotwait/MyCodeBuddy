/**
 * Coming back to a canvas card after the route took it away, from the runtime
 * store's side.
 *
 * The canvas is a full-page route, so switching to the tasks or token page
 * UNMOUNTS every expanded card while the workspace's own tabs are only hidden.
 * A card is therefore the one live-conversation surface that regularly goes
 * away while its conversation keeps working — and the runtime session it leaves
 * behind does not keep up with it:
 *
 *   - `liveMessage` is written by a sink the CARD registers, so it freezes at
 *     whatever had streamed in by the time the card unmounted;
 *   - the global `turn_complete` handler (which promotes turns for any
 *     conversation with no tab open — every canvas card qualifies) supplies
 *     the provider-authoritative final message to `localTurns`, then the
 *     settled detail refetch replaces that local copy with the persisted
 *     transcript;
 *   - a turn that ends without a completion this client sees — the agent dies,
 *     the connection is reclaimed — leaves `awaiting_persist` pinned, and with
 *     it the optimistic user turn, on top of the persisted copy of the same
 *     message;
 *   - and `useConversationDetail` never re-fetches a detail it already has, so
 *     none of it heals on its own.
 *
 * These pin the repair the surface runs on re-entry: unpin `awaiting_persist`
 * when this card's connection is not the one prompting, then refetch and let
 * the database be authoritative — without ever eating a reply that is still
 * streaming.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

import type { LiveMessage } from "@/contexts/acp-connections-context"
import {
  resetConversationRuntimeStore,
  selectTimelineTurns,
  useConversationRuntimeStore,
} from "@/stores/conversation-runtime-store"
import type { DbConversationDetail, MessageTurn } from "@/lib/types"

vi.mock("@/lib/api", () => ({
  getFolderConversation: vi.fn(),
  getFolderConversationTurns: vi.fn(),
}))

const { getFolderConversation } = await import("@/lib/api")
const mockGetFolderConversation = vi.mocked(getFolderConversation)

const CONV = 77

/** The round that was already on screen when the user expanded the card. */
const OLD_USER: MessageTurn = {
  id: "turn-1",
  role: "user",
  blocks: [{ type: "text", text: "what does the worker do on startup?" }],
  timestamp: "2026-09-02T08:00:00.000Z",
}
const OLD_REPLY: MessageTurn = {
  id: "turn-2",
  role: "assistant",
  blocks: [{ type: "text", text: "It opens the queue and waits." }],
  timestamp: "2026-09-02T08:00:04.000Z",
}

/** What the user typed into the card just before leaving the canvas. */
const SENT: MessageTurn = {
  id: "optimistic-9",
  role: "user",
  blocks: [{ type: "text", text: "add a health check to it" }],
  timestamp: "2026-09-02T09:00:00.000Z",
}

/** As much of the reply as had streamed in before the card unmounted. */
const PARTIAL: LiveMessage = {
  id: "live-9",
  role: "assistant",
  content: [{ type: "text", text: "Adding a health" }],
  startedAt: Date.parse("2026-09-02T09:00:01.000Z"),
}
const FINAL: LiveMessage = {
  id: PARTIAL.id,
  role: "assistant",
  content: [
    {
      type: "text",
      text: "Adding a health check endpoint and wiring it into the poller.",
    },
  ],
  startedAt: PARTIAL.startedAt,
}

/** The persisted copies of that round, written while nobody was watching. */
const PERSISTED_SENT: MessageTurn = {
  id: "turn-3",
  role: "user",
  blocks: [{ type: "text", text: "add a health check to it" }],
  timestamp: "2026-09-02T09:00:00.000Z",
}
const PERSISTED_REPLY: MessageTurn = {
  id: "turn-4",
  role: "assistant",
  blocks: [
    {
      type: "text",
      text: "Adding a health check endpoint and wiring it into the poller.",
    },
  ],
  timestamp: "2026-09-02T09:00:30.000Z",
}

function actions() {
  return useConversationRuntimeStore.getState().actions
}

function session() {
  return useConversationRuntimeStore.getState().byConversationId.get(CONV)
}

function timelineTexts(): string[] {
  return selectTimelineTurns(useConversationRuntimeStore.getState(), CONV).map(
    (item) =>
      item.turn.blocks
        .map((block) => (block.type === "text" ? block.text : ""))
        .join("")
  )
}

function detail(
  turns: MessageTurn[],
  options?: { inFlightUserTurnId?: string }
): DbConversationDetail {
  return {
    summary: {
      id: CONV,
      folder_id: 1,
      title: "worker startup",
      title_locked: false,
      auto_title_finalized: false,
      agent_type: "claude_code",
      status: options?.inFlightUserTurnId ? "in_progress" : "completed",
      awaiting_reply_token: null,
      kind: "regular",
      model: null,
      git_branch: null,
      external_id: "sess-worker",
      message_count: turns.length,
      child_count: 0,
      created_at: "2026-09-02T08:00:00.000Z",
      updated_at: "2026-09-02T09:00:30.000Z",
      pinned_at: null,
    },
    turns,
    session_stats: null,
    in_flight_user_turn_id: options?.inFlightUserTurnId ?? null,
  }
}

async function loadDetail(next: DbConversationDetail) {
  mockGetFolderConversation.mockResolvedValueOnce(next)
  actions().refetchDetail(CONV)
  await Promise.resolve()
  await Promise.resolve()
  await Promise.resolve()
}

/**
 * The card as the user left it: an earlier round already fetched, their new
 * message on screen, and part of the answer streamed in.
 */
async function seedCardMidSendThenLeave() {
  await loadDetail(detail([OLD_USER, OLD_REPLY]))
  actions().appendOptimisticTurn(CONV, SENT, SENT.id)
  actions().setSyncState(CONV, "awaiting_persist")
  actions().setLiveMessage(CONV, PARTIAL, true)
  // Unmounting is not an event the store hears about; it just stops writing.
}

beforeEach(() => {
  resetConversationRuntimeStore()
  mockGetFolderConversation.mockReset()
  mockGetFolderConversation.mockImplementation(() => new Promise(() => {}))
})

afterEach(() => {
  resetConversationRuntimeStore()
})

describe("a turn that finished while the card was off screen", () => {
  it("promotes the final live message supplied at completion", async () => {
    await seedCardMidSendThenLeave()

    // The global completion handler carries the final live message even though
    // the card's own sink stopped receiving updates when it unmounted.
    actions().completeTurn(CONV, FINAL)

    expect(timelineTexts()).toContain(
      "Adding a health check endpoint and wiring it into the poller."
    )
    expect(timelineTexts()).not.toContain("Adding a health")
  })

  it("is replaced by the persisted reply once the card refetches", async () => {
    await seedCardMidSendThenLeave()
    actions().completeTurn(CONV, FINAL)

    // Completion landed syncState back on idle, so the settled detail is
    // authoritative and clears every local buffer with it.
    expect(session()?.syncState).toBe("idle")
    await loadDetail(
      detail([OLD_USER, OLD_REPLY, PERSISTED_SENT, PERSISTED_REPLY])
    )

    expect(timelineTexts()).toEqual([
      "what does the worker do on startup?",
      "It opens the queue and waits.",
      "add a health check to it",
      "Adding a health check endpoint and wiring it into the poller.",
    ])
    expect(session()?.localTurns).toEqual([])
    expect(session()?.liveMessage).toBeNull()
  })
})

describe("a turn that died while the card was off screen", () => {
  it("keeps its stale buffers through a refetch while awaiting_persist is pinned", async () => {
    await seedCardMidSendThenLeave()

    // No completion ever arrives (the agent died, or the idle sweep reclaimed
    // the connection). `awaiting_persist` is what a live send hides behind, so
    // the refetch preserves the very buffers that are now wrong: the user's
    // message renders twice and the truncated stream sits under it.
    await loadDetail(
      detail([OLD_USER, OLD_REPLY, PERSISTED_SENT, PERSISTED_REPLY])
    )

    expect(session()?.syncState).toBe("awaiting_persist")
    const texts = timelineTexts()
    expect(texts.filter((t) => t === "add a health check to it")).toHaveLength(
      2
    )
    expect(texts).toContain("Adding a health")
  })

  it("settles once the card unpins it and refetches", async () => {
    await seedCardMidSendThenLeave()

    // What the surface does on re-entry when its own connection is not the one
    // prompting: the send it was waiting on is over, whatever happened to it.
    actions().setSyncState(CONV, "idle")
    await loadDetail(
      detail([OLD_USER, OLD_REPLY, PERSISTED_SENT, PERSISTED_REPLY])
    )

    expect(timelineTexts()).toEqual([
      "what does the worker do on startup?",
      "It opens the queue and waits.",
      "add a health check to it",
      "Adding a health check endpoint and wiring it into the poller.",
    ])
    expect(session()?.optimisticTurns).toEqual([])
    expect(session()?.liveMessage).toBeNull()
  })

  it("no longer accepts the settled replay a reconnect pushes at it", async () => {
    await seedCardMidSendThenLeave()
    actions().setSyncState(CONV, "idle")
    await loadDetail(
      detail([OLD_USER, OLD_REPLY, PERSISTED_SENT, PERSISTED_REPLY])
    )

    // `registerLiveMessageSink` replays the connection's retained live message
    // on every mount. With the pin gone, SET_LIVE_MESSAGE's guard rejects a
    // non-live replay over an existing transcript — which is the other half of
    // why unpinning has to happen before the card's sink registers.
    actions().setLiveMessage(CONV, PARTIAL, false)

    expect(session()?.liveMessage).toBeNull()
    expect(timelineTexts()).not.toContain("Adding a health")
  })
})

describe("coming back while the reply is still streaming", () => {
  it("survives the re-entry refetch untouched", async () => {
    await seedCardMidSendThenLeave()

    // The backend stamps `in_flight_user_turn_id` for as long as the turn runs,
    // and the reducer keeps every live buffer for such a detail. So even if the
    // card mistakes a turn running elsewhere for a finished one and unpins,
    // the refetch cannot clear a reply that is still coming in.
    actions().setSyncState(CONV, "idle")
    await loadDetail(
      detail([OLD_USER, OLD_REPLY, PERSISTED_SENT], {
        inFlightUserTurnId: PERSISTED_SENT.id,
      })
    )

    expect(session()?.liveMessage?.id).toBe(PARTIAL.id)
    expect(session()?.optimisticTurns).toHaveLength(1)
    expect(timelineTexts()).toContain("Adding a health")
  })
})
