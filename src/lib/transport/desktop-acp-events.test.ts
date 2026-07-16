import { describe, expect, it, vi } from "vitest"
import type {
  DesktopAcpEventHandlers,
  DesktopDeliveryCapabilities,
  DesktopDeliveryMode,
} from "@/lib/types"
import { subscribeDesktopAcpEvents } from "./desktop-acp-events"

const handlers: DesktopAcpEventHandlers = {
  onBatch: vi.fn(),
  onFailure: vi.fn(),
}

function capabilities(mode: DesktopDeliveryMode): DesktopDeliveryCapabilities {
  const batching = mode === "batched"
  return {
    mode,
    flags: {
      desktop_acp_event_batching: batching,
      incremental_live_transcript: false,
      deferred_streaming_rich_content: false,
    },
    perf_replay_available: false,
    failure_event: "acp://delivery-failed",
  }
}

function fakeTransport() {
  const events: string[] = []
  let unsubscribeCount = 0
  return {
    subscribe: vi.fn(async (event: string) => {
      events.push(event)
      return () => {
        unsubscribeCount += 1
      }
    }),
    subscribedEvents: () => events.slice(),
    subscribedDataEvents: () =>
      events.filter((event) => event !== "acp://delivery-failed"),
    unsubscribeCount: () => unsubscribeCount,
  }
}

describe("subscribeDesktopAcpEvents", () => {
  it.each([
    ["legacy", "acp://event"],
    ["batched", "acp://event-batch"],
  ] as const)("subscribes %s mode to only %s", async (mode, expectedEvent) => {
    const transport = fakeTransport()
    const unsubscribe = await subscribeDesktopAcpEvents(
      capabilities(mode),
      handlers,
      transport
    )
    expect(transport.subscribedDataEvents()).toEqual([expectedEvent])
    expect(transport.subscribedEvents().includes("acp://delivery-failed")).toBe(
      mode === "batched"
    )
    unsubscribe()
    expect(transport.unsubscribeCount()).toBe(
      transport.subscribedEvents().length
    )
  })

  it("wraps legacy envelopes as one-event batches with monotonic ids", async () => {
    const transport = fakeTransport()
    type Handler = (payload: unknown) => void
    const handlersByEvent = new Map<string, Handler>()
    transport.subscribe.mockImplementation(
      async (event: string, handler: Handler) => {
        handlersByEvent.set(event, handler)
        return () => undefined
      }
    )
    const onBatch = vi.fn()
    await subscribeDesktopAcpEvents(
      capabilities("legacy"),
      { onBatch, onFailure: vi.fn() },
      transport
    )
    const legacyHandler = handlersByEvent.get("acp://event")
    expect(legacyHandler).toBeTypeOf("function")
    legacyHandler?.({
      connection_id: "c1",
      seq: 1,
      type: "content_delta",
      text: "a",
    })
    legacyHandler?.({
      connection_id: "c1",
      seq: 2,
      type: "content_delta",
      text: "b",
    })
    expect(onBatch).toHaveBeenCalledTimes(2)
    expect(onBatch.mock.calls[0][0]).toEqual({
      batch_id: 1,
      events: [
        {
          connection_id: "c1",
          seq: 1,
          type: "content_delta",
          text: "a",
        },
      ],
    })
    expect(onBatch.mock.calls[1][0].batch_id).toBe(2)
  })
})
