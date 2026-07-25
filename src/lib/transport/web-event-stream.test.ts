import { beforeEach, describe, expect, it, vi } from "vitest"
import type { LiveSessionSnapshot } from "@/lib/types"
import { WebEventStream, type AttachTransportHost } from "./web-event-stream"

function snapshot(eventSeq: number): LiveSessionSnapshot {
  return {
    connection_id: "conn",
    conversation_id: 42,
    folder_id: 1,
    status: "connected",
    external_id: "sid",
    live_message: null,
    active_tool_calls: [],
    pending_permission: null,
    modes: null,
    current_mode: null,
    config_options: null,
    prompt_capabilities: null,
    usage: null,
    fork_supported: false,
    available_commands: [],
    selectors_ready: true,
    event_seq: eventSeq,
  }
}

function hostFixture() {
  let ready: (() => void) | null = null
  const sendFrame = vi.fn(() => true)
  const host: AttachTransportHost = {
    isWsOpen: () => true,
    sendFrame,
    onWsReady: (callback) => {
      ready = callback
      return () => {
        ready = null
      }
    },
  }
  return { host, sendFrame, reconnect: () => ready?.() }
}

const handlers = {
  onSnapshot: vi.fn(),
  onReplay: vi.fn(),
  onEvent: vi.fn(),
  onDetached: vi.fn(),
}

describe("WebEventStream reconnect mode", () => {
  beforeEach(() => vi.clearAllMocks())

  it("resumes an ordinary subscription from its last applied seq", () => {
    const f = hostFixture()
    const stream = new WebEventStream(f.host)
    const sub = stream.attach("conn", {}, handlers)
    stream.handleServerFrame({
      type: "snapshot",
      subscription_id: sub.subscriptionId,
      connection_id: "conn",
      snapshot: snapshot(11),
      event_seq: 11,
    })
    f.sendFrame.mockClear()
    f.reconnect()
    expect(f.sendFrame).toHaveBeenCalledWith(
      expect.objectContaining({ action: "attach", since_seq: 11 })
    )
  })

  it("cold-reattaches a delegate observer even after applying events", () => {
    const f = hostFixture()
    const stream = new WebEventStream(f.host)
    const sub = stream.attach("conn", { reconnectMode: "cold" }, handlers)
    stream.handleServerFrame({
      type: "event",
      subscription_id: sub.subscriptionId,
      envelope: { seq: 12, connection_id: "conn", type: "turn_complete" },
    })
    f.sendFrame.mockClear()
    f.reconnect()
    expect(f.sendFrame).toHaveBeenCalledWith(
      expect.objectContaining({ action: "attach", since_seq: undefined })
    )
  })
})
