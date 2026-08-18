import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

const tauri = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
  emitFrame: null as ((payload: unknown) => void) | null,
}))

vi.mock("@tauri-apps/api/core", () => ({
  invoke: tauri.invoke,
}))

vi.mock("@tauri-apps/api/event", () => ({
  listen: tauri.listen,
}))

import { RemoteDesktopTransport } from "./remote-desktop-transport"

const handlers = {
  onSnapshot: vi.fn(),
  onReplay: vi.fn(),
  onEvent: vi.fn(),
  onDetached: vi.fn(),
}

function sentFrames(): object[] {
  return tauri.invoke.mock.calls
    .filter(([command]) => command === "remote_ws_send_text")
    .map(([, args]) => JSON.parse((args as { text: string }).text) as object)
}

describe("RemoteDesktopTransport shared event stream", () => {
  beforeEach(() => {
    vi.useFakeTimers()
    tauri.invoke.mockReset().mockResolvedValue(undefined)
    tauri.listen.mockReset().mockImplementation((_event, handler) => {
      tauri.emitFrame = (payload) => handler({ payload })
      return Promise.resolve(vi.fn())
    })
    tauri.emitFrame = null
  })

  afterEach(() => {
    vi.useRealTimers()
  })

  it("forwards fenced attach and application ping frames unchanged", async () => {
    const transport = new RemoteDesktopTransport({
      id: 9,
      name: "remote",
      baseUrl: "http://remote.test",
      token: "token",
      windowInstanceId: "window-1",
    })
    const stream = transport.eventStream()
    await Promise.resolve()
    tauri.emitFrame?.({ channel: "__ready__", payload: null })

    stream.attach(
      "connection-9",
      { shared: { generation: 9, leaseId: "lease-9" } },
      handlers
    )
    expect(sentFrames()).toContainEqual(
      expect.objectContaining({
        action: "attach",
        connection_id: "connection-9",
        generation: 9,
        lease_id: "lease-9",
      })
    )

    tauri.invoke.mockClear()
    vi.advanceTimersByTime(30_000)

    expect(sentFrames()).toEqual([{ action: "ping" }])
    transport.destroy()
  })
})
