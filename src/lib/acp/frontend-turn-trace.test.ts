import { beforeEach, describe, expect, it, vi } from "vitest"

const h = vi.hoisted(() => ({
  call: vi.fn(),
}))

vi.mock("@/lib/transport", () => ({
  getTransport: () => ({ call: h.call }),
}))

import { recordFrontendTurnTrace } from "./frontend-turn-trace"

describe("recordFrontendTurnTrace", () => {
  beforeEach(() => {
    h.call.mockReset()
    vi.restoreAllMocks()
  })

  it("adds wall-clock time and forwards only structured milestone metadata", () => {
    vi.spyOn(Date, "now").mockReturnValue(1_234)
    h.call.mockResolvedValue(undefined)

    recordFrontendTurnTrace({
      phase: "prompting_frame",
      contextKey: "tab-1",
      connectionId: "conn-1",
      conversationId: 42,
      liveMessageId: "live-1",
      eventSeq: 7,
      receivedAtMs: 120,
      elapsedMs: 4,
      sinkRegistered: true,
    })

    expect(h.call).toHaveBeenCalledWith("record_frontend_turn_trace", {
      trace: {
        phase: "prompting_frame",
        clientTimestampMs: 1_234,
        contextKey: "tab-1",
        connectionId: "conn-1",
        conversationId: 42,
        liveMessageId: "live-1",
        eventSeq: 7,
        receivedAtMs: 120,
        elapsedMs: 4,
        sinkRegistered: true,
      },
    })
    expect(recordFrontendTurnTrace({ phase: "send_started" })).toBeUndefined()
  })

  it("never rejects the caller when diagnostic persistence fails", async () => {
    h.call.mockRejectedValue(new Error("backend unavailable"))

    expect(() =>
      recordFrontendTurnTrace({ phase: "send_started" })
    ).not.toThrow()
    await Promise.resolve()
  })

  it("does not throw when the transport fails synchronously", () => {
    h.call.mockImplementation(() => {
      throw new Error("transport unavailable")
    })

    expect(() =>
      recordFrontendTurnTrace({ phase: "send_started" })
    ).not.toThrow()
  })
})
