import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

const randomUUID = vi.hoisted(() => vi.fn<() => string>())

vi.mock("@/lib/utils", () => ({ randomUUID }))

describe("shared ACP client identity", () => {
  let nextUuid = 0

  beforeEach(() => {
    localStorage.clear()
    nextUuid = 0
    randomUUID
      .mockReset()
      .mockImplementation(
        () => `00000000-0000-4000-8000-${String(++nextUuid).padStart(12, "0")}`
      )
  })

  afterEach(() => {
    vi.restoreAllMocks()
    vi.resetModules()
  })

  it("persists the device id while each document gets a new client id", async () => {
    const firstModule = await import("./shared-session-client")
    const first = firstModule.getSharedClientIdentity()

    expect(localStorage.getItem("codeg.sharedSession.deviceId.v1")).toBe(
      first.deviceId
    )

    vi.resetModules()
    const secondModule = await import("./shared-session-client")
    const second = secondModule.getSharedClientIdentity()

    expect(second.deviceId).toBe(first.deviceId)
    expect(second.clientInstanceId).not.toBe(first.clientInstanceId)
  })

  it("replaces an invalid persisted device id across document reloads", async () => {
    const invalidDeviceId = "not-a-uuid".repeat(64)
    localStorage.setItem("codeg.sharedSession.deviceId.v1", invalidDeviceId)

    const firstModule = await import("./shared-session-client")
    const first = firstModule.getSharedClientIdentity()

    expect(first.deviceId).not.toBe(invalidDeviceId)
    expect(first.deviceId).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i
    )
    expect(localStorage.getItem("codeg.sharedSession.deviceId.v1")).toBe(
      first.deviceId
    )

    vi.resetModules()
    const secondModule = await import("./shared-session-client")
    const second = secondModule.getSharedClientIdentity()

    expect(second.deviceId).toBe(first.deviceId)
    expect(second.clientInstanceId).not.toBe(first.clientInstanceId)
  })

  it("keeps document identity stable and creates fresh request ids", async () => {
    const sharedClient = await import("./shared-session-client")

    expect(sharedClient.getSharedClientIdentity()).toEqual(
      sharedClient.getSharedClientIdentity()
    )
    expect(sharedClient.newSharedRequestId()).not.toBe(
      sharedClient.newSharedRequestId()
    )
  })

  it.each(["read", "write"] as const)(
    "falls back to bounded memory when storage %s throws",
    async (operation) => {
      if (operation === "read") {
        vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
          throw new DOMException("blocked", "SecurityError")
        })
      } else {
        vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
          throw new DOMException("full", "QuotaExceededError")
        })
      }

      const sharedClient = await import("./shared-session-client")
      const first = sharedClient.getSharedClientIdentity()
      const second = sharedClient.getSharedClientIdentity()

      expect(second).toEqual(first)
      expect(first.deviceId).toMatch(
        /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i
      )
    }
  )
})
