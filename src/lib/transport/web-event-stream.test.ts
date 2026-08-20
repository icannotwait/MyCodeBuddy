import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
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
  let wsOpen = true
  const sendFrame = vi.fn(() => true)
  const host: AttachTransportHost = {
    isWsOpen: () => wsOpen,
    sendFrame,
    onWsReady: (callback) => {
      ready = callback
      return () => {
        ready = null
      }
    },
  }
  return {
    host,
    sendFrame,
    reconnect: () => ready?.(),
    setOpen: (open: boolean) => {
      wsOpen = open
    },
  }
}

const handlers = {
  onSnapshot: vi.fn(),
  onReplay: vi.fn(),
  onEvent: vi.fn(),
  onDetached: vi.fn(),
  onAttachError: vi.fn(),
}

describe("WebEventStream reconnect mode", () => {
  beforeEach(() => vi.clearAllMocks())
  afterEach(() => vi.useRealTimers())

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

  it("reattaches with the same generation and lease", () => {
    const f = hostFixture()
    const stream = new WebEventStream(f.host)
    stream.attach(
      "conn",
      { shared: { generation: 4, leaseId: "lease-4" } },
      handlers
    )

    expect(f.sendFrame).toHaveBeenLastCalledWith(
      expect.objectContaining({
        action: "attach",
        connection_id: "conn",
        generation: 4,
        lease_id: "lease-4",
      })
    )

    f.sendFrame.mockClear()
    f.reconnect()

    expect(f.sendFrame).toHaveBeenCalledWith(
      expect.objectContaining({ generation: 4, lease_id: "lease-4" })
    )
  })

  it("pings every 30 seconds only while a shared subscription exists", () => {
    vi.useFakeTimers()
    const f = hostFixture()
    const stream = new WebEventStream(f.host)
    const first = stream.attach(
      "conn",
      { shared: { generation: 4, leaseId: "lease-4" } },
      handlers
    )
    f.sendFrame.mockClear()

    vi.advanceTimersByTime(29_999)
    expect(f.sendFrame).not.toHaveBeenCalledWith({ action: "ping" })
    vi.advanceTimersByTime(1)
    expect(f.sendFrame).toHaveBeenCalledWith({ action: "ping" })

    f.sendFrame.mockClear()
    first.detach()
    f.sendFrame.mockClear()
    vi.advanceTimersByTime(60_000)
    expect(f.sendFrame).not.toHaveBeenCalledWith({ action: "ping" })

    stream.destroy()
  })

  it("keeps exactly one heartbeat across shared subscription lifecycle", () => {
    vi.useFakeTimers()
    const f = hostFixture()
    const stream = new WebEventStream(f.host)
    const first = stream.attach(
      "first",
      { shared: { generation: 4, leaseId: "lease-4" } },
      handlers
    )
    const second = stream.attach(
      "second",
      { shared: { generation: 5, leaseId: "lease-5" } },
      handlers
    )
    f.sendFrame.mockClear()

    vi.advanceTimersByTime(30_000)
    expect(f.sendFrame).toHaveBeenCalledTimes(1)
    expect(f.sendFrame).toHaveBeenCalledWith({ action: "ping" })

    first.detach()
    f.sendFrame.mockClear()
    f.reconnect()
    f.sendFrame.mockClear()
    vi.advanceTimersByTime(30_000)
    expect(f.sendFrame).toHaveBeenCalledTimes(1)
    expect(f.sendFrame).toHaveBeenCalledWith({ action: "ping" })

    f.setOpen(false)
    f.sendFrame.mockClear()
    vi.advanceTimersByTime(30_000)
    expect(f.sendFrame).not.toHaveBeenCalled()
    f.setOpen(true)

    second.detach()
    f.sendFrame.mockClear()
    vi.advanceTimersByTime(60_000)
    expect(f.sendFrame).not.toHaveBeenCalledWith({ action: "ping" })

    const destroyFixture = hostFixture()
    const destroyStream = new WebEventStream(destroyFixture.host)
    destroyStream.attach(
      "destroyed",
      { shared: { generation: 6, leaseId: "lease-6" } },
      handlers
    )
    destroyFixture.sendFrame.mockClear()
    destroyStream.destroy()
    vi.advanceTimersByTime(60_000)
    expect(destroyFixture.sendFrame).not.toHaveBeenCalled()

    const legacyFixture = hostFixture()
    const legacyStream = new WebEventStream(legacyFixture.host)
    legacyStream.attach("legacy", {}, handlers)
    legacyFixture.sendFrame.mockClear()
    vi.advanceTimersByTime(60_000)
    expect(legacyFixture.sendFrame).not.toHaveBeenCalled()
    legacyStream.destroy()
    stream.destroy()
  })

  it("treats a lease-expired server detach as terminal", () => {
    vi.useFakeTimers()
    const f = hostFixture()
    const stream = new WebEventStream(f.host)
    const sub = stream.attach(
      "conn",
      { shared: { generation: 4, leaseId: "lease-4" } },
      handlers
    )
    f.sendFrame.mockClear()

    stream.handleServerFrame({
      type: "detached",
      subscription_id: sub.subscriptionId,
      reason: "lease_expired",
    })
    f.reconnect()
    vi.advanceTimersByTime(60_000)

    expect(handlers.onDetached).toHaveBeenCalledWith("lease_expired")
    expect(f.sendFrame).not.toHaveBeenCalled()
    stream.destroy()
  })

  it("sends detach before dropping a failed attach subscription", () => {
    const f = hostFixture()
    const stream = new WebEventStream(f.host)
    const sub = stream.attach("conn", {}, handlers)
    f.sendFrame.mockClear()

    stream.notifyOversizedFrame(sub.subscriptionId)

    expect(f.sendFrame).toHaveBeenCalledWith({
      action: "detach",
      subscription_id: sub.subscriptionId,
    })
    expect(handlers.onAttachError).toHaveBeenCalledWith("oversized_frame", true)
    expect(handlers.onDetached).not.toHaveBeenCalled()
  })

  it("sends detach for a snapshot_budget_exceeded attach_error frame", () => {
    const f = hostFixture()
    const stream = new WebEventStream(f.host)
    const sub = stream.attach("conn", {}, handlers)
    f.sendFrame.mockClear()

    stream.handleServerFrame({
      type: "attach_error",
      subscription_id: sub.subscriptionId,
      code: "snapshot_budget_exceeded",
    })

    expect(f.sendFrame).toHaveBeenCalledWith({
      action: "detach",
      subscription_id: sub.subscriptionId,
    })
    expect(handlers.onAttachError).toHaveBeenCalledWith(
      "snapshot_budget_exceeded",
      true
    )
  })
})
