import { act, renderHook } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

import type {
  ConnectionLiveSinks,
  ConnectionState,
  LiveMessage,
} from "@/contexts/acp-connections-context"
import {
  resetConversationRuntimeStore,
  useConversationRuntimeStore,
} from "@/stores/conversation-runtime-store"
import {
  __resetLiveTranscriptStoreForTests,
  useLiveTranscriptConversation,
} from "@/stores/live-transcript-store"
import { useLiveTranscriptBridge } from "./live-transcript-view"

const h = vi.hoisted(() => ({
  registerLiveSinks: vi.fn(),
}))

vi.mock("@/contexts/acp-connections-context", async () => {
  const actual = await vi.importActual<
    typeof import("@/contexts/acp-connections-context")
  >("@/contexts/acp-connections-context")
  return {
    ...actual,
    useAcpActions: () => ({ registerLiveSinks: h.registerLiveSinks }),
  }
})

const RUNTIME_ID = -42

function liveMessage(): LiveMessage {
  return {
    id: "viewer-reply",
    role: "assistant",
    content: [{ type: "text", text: "visible reply" }],
    startedAt: 1_700_000_000_000,
  }
}

function connection(
  status: ConnectionState["status"],
  message: LiveMessage,
  acceptedCompletionMessageId: string | null = null,
  acceptedCompletionRuntimeConversationIds: readonly number[] | null = null
): ConnectionState & {
  acceptedCompletionMessageId: string | null
  acceptedCompletionRuntimeConversationIds: readonly number[] | null
} {
  return {
    connectionId: "viewer-connection",
    status,
    liveMessage: message,
    acceptedCompletionMessageId,
    acceptedCompletionRuntimeConversationIds,
  } as ConnectionState & {
    acceptedCompletionMessageId: string | null
    acceptedCompletionRuntimeConversationIds: readonly number[] | null
  }
}

describe("useLiveTranscriptBridge completion ownership", () => {
  beforeEach(() => {
    resetConversationRuntimeStore()
    __resetLiveTranscriptStoreForTests()
    h.registerLiveSinks.mockReset()
    h.registerLiveSinks.mockReturnValue(() => {})
  })

  it("registers its runtime and does not complete on a status-only edge", () => {
    const message = liveMessage()
    const actions = useConversationRuntimeStore.getState().actions
    actions.setExternalId(RUNTIME_ID, "viewer-session")
    actions.setLiveMessage(RUNTIME_ID, message, true)

    const { rerender } = renderHook(
      ({ conn }) => useLiveTranscriptBridge(RUNTIME_ID, conn),
      { initialProps: { conn: connection("prompting", message) } }
    )

    expect(h.registerLiveSinks).toHaveBeenCalledWith(
      "viewer-connection",
      expect.objectContaining({ runtimeConversationId: RUNTIME_ID })
    )

    rerender({ conn: connection("error", message) })

    const runtime = useConversationRuntimeStore
      .getState()
      .byConversationId.get(RUNTIME_ID)
    expect(runtime?.localTurns).toEqual([])
    expect(runtime?.liveMessage).toBe(message)
  })

  it("does not adopt a settled reply outside provider registration", () => {
    const message = liveMessage()
    useConversationRuntimeStore
      .getState()
      .actions.setExternalId(RUNTIME_ID, "viewer-session")

    const { rerender } = renderHook(
      ({ conn }) => useLiveTranscriptBridge(RUNTIME_ID, conn),
      { initialProps: { conn: connection("connected", message) } }
    )

    expect(
      useConversationRuntimeStore.getState().byConversationId.get(RUNTIME_ID)
        ?.localTurns
    ).toEqual([])

    rerender({
      conn: connection("connected", message, message.id, [999]),
    })

    expect(
      useConversationRuntimeStore.getState().byConversationId.get(RUNTIME_ID)
        ?.localTurns
    ).toEqual([])

    rerender({
      conn: connection("connected", message, message.id, [RUNTIME_ID]),
    })

    expect(
      useConversationRuntimeStore.getState().byConversationId.get(RUNTIME_ID)
        ?.localTurns
    ).toEqual([])
  })

  it.each([false, true])(
    "keeps a sibling consumer's shared reply when a viewer closes (completed=%s)",
    (completed) => {
      const conversationId = 42
      const message = liveMessage()
      const actions = useConversationRuntimeStore.getState().actions
      actions.setLiveMessage(conversationId, message, true)
      if (completed) actions.completeTurn(conversationId, message)
      const sibling = renderHook(() =>
        useConversationRuntimeStore((state) =>
          state.byConversationId.get(conversationId)
        )
      )
      const retained = sibling.result.current
      expect(retained).toBeDefined()
      const dispose = vi.fn()
      h.registerLiveSinks.mockReturnValue(dispose)
      const viewer = renderHook(() =>
        useLiveTranscriptBridge(
          conversationId,
          connection(completed ? "connected" : "prompting", message)
        )
      )

      viewer.unmount()

      expect(sibling.result.current).toBe(retained)
      expect(dispose).toHaveBeenCalledOnce()
      if (completed) {
        expect(sibling.result.current?.localTurns[0]?.blocks).toEqual([
          { type: "text", text: "visible reply" },
        ])
      } else {
        expect(sibling.result.current?.liveMessage).toBe(message)
      }
    }
  )

  it("projects readonly live frames without a main session surface", () => {
    const message = liveMessage()
    const { result } = renderHook(() => {
      useLiveTranscriptBridge(RUNTIME_ID, connection("prompting", message))
      return useLiveTranscriptConversation(RUNTIME_ID)
    })
    const sinks = h.registerLiveSinks.mock.calls[0][1] as ConnectionLiveSinks

    act(() => {
      sinks.canonical(message, true)
      sinks.transcript?.rebuild(message, 1)
    })

    expect(result.current?.connectionId).toBe("viewer-connection")
    expect([...result.current!.segments.values()]).toEqual([
      expect.objectContaining({ type: "text", text: "visible reply" }),
    ])

    const updated: LiveMessage = {
      ...message,
      content: [{ type: "text", text: "visible reply continued" }],
    }
    act(() => {
      sinks.canonical(updated, true)
      sinks.transcript?.publish(
        {
          contextKey: "viewer-connection",
          connectionId: "viewer-connection",
          deliveryIds: [2],
          highestSeq: 2,
          applyEvents: [
            {
              type: "content_delta",
              connection_id: "viewer-connection",
              seq: 2,
              text: " continued",
            },
          ],
          rawEvents: [],
        },
        updated
      )
    })
    expect([...result.current!.segments.values()]).toEqual([
      expect.objectContaining({
        type: "text",
        text: "visible reply continued",
      }),
    ])
    expect(
      useConversationRuntimeStore.getState().byConversationId.get(RUNTIME_ID)
        ?.liveMessage
    ).toBe(updated)
  })
})
