import { renderHook } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

import type {
  ConnectionState,
  LiveMessage,
} from "@/contexts/acp-connections-context"
import {
  resetConversationRuntimeStore,
  useConversationRuntimeStore,
} from "@/stores/conversation-runtime-store"

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
    h.registerLiveSinks.mockReset()
    h.registerLiveSinks.mockReturnValue(() => {})
  })

  it("registers its runtime and does not complete on a status-only edge", async () => {
    const { useLiveTranscriptBridge } = await import("./live-transcript-view")
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

  it("does not adopt a settled reply outside provider registration", async () => {
    const { useLiveTranscriptBridge } = await import("./live-transcript-view")
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
})
