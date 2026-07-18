import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

const h = vi.hoisted(() => ({
  detectEnvironment: vi.fn(() => "tauri" as "tauri" | "web"),
}))

vi.mock("./detect", () => ({
  detectEnvironment: h.detectEnvironment,
}))

import {
  __resetTransportForTests,
  __setRemoteDesktopForTests,
  getActiveBackendCacheKey,
} from "./index"
import type { Transport } from "./types"

const stubTransport = (): Transport => ({
  call: vi.fn(),
  subscribe: vi.fn(async () => () => {}),
  isDesktop: () => true,
  destroy: vi.fn(),
})

describe("getActiveBackendCacheKey", () => {
  beforeEach(() => {
    __resetTransportForTests()
    h.detectEnvironment.mockReset()
    h.detectEnvironment.mockReturnValue("tauri")
  })

  afterEach(() => {
    __resetTransportForTests()
  })

  it('returns "local:tauri" for the desktop shell', () => {
    h.detectEnvironment.mockReturnValue("tauri")
    expect(getActiveBackendCacheKey()).toBe("local:tauri")
  })

  it("returns web:${origin} for the browser shell", () => {
    h.detectEnvironment.mockReturnValue("web")
    expect(getActiveBackendCacheKey()).toBe(`web:${window.location.origin}`)
  })

  it("returns remote:${id} when a remote-desktop connection is active", () => {
    h.detectEnvironment.mockReturnValue("tauri")
    __setRemoteDesktopForTests(
      {
        id: 42,
        name: "remote",
        baseUrl: "http://remote.example",
        token: "tok",
        windowInstanceId: "win-1",
      },
      stubTransport()
    )
    expect(getActiveBackendCacheKey()).toBe("remote:42")
  })
})
