import { beforeEach, describe, expect, it, vi } from "vitest"

const mocks = vi.hoisted(() => ({
  transportCall: vi.fn(),
  shellCall: vi.fn<(command: string, args?: unknown) => Promise<unknown>>(
    async () => undefined
  ),
  remoteCall: vi.fn(async () => undefined),
  isDesktop: vi.fn(() => true),
}))

vi.mock("./transport", () => ({
  getTransport: () => ({ call: mocks.transportCall }),
  getShellTransport: () => ({ call: mocks.shellCall }),
  isDesktop: () => mocks.isDesktop(),
}))

vi.mock("@/lib/utils", () => ({
  randomUUID: () => "test-action-uuid",
}))

import {
  deliverSystemNotification,
  getNotificationIdentity,
  getNotificationPermission,
  openSystemNotificationSettings,
  requestNotificationPermission,
  sendSystemNotification,
} from "@/lib/notification"

interface FakeNotificationCtor {
  (title: string, options?: { body?: string }): void
  permission: string
  requestPermission: () => Promise<string>
}

/** Install a browser `Notification` with the given permission state. */
function installNotification(
  permission: string,
  requestResult = permission
): { constructed: Array<[string, { body?: string } | undefined]> } {
  const constructed: Array<[string, { body?: string } | undefined]> = []
  const ctor = function (title: string, options?: { body?: string }) {
    constructed.push([title, options])
  } as unknown as FakeNotificationCtor
  ctor.permission = permission
  ctor.requestPermission = vi.fn(async () => {
    ctor.permission = requestResult
    return requestResult
  })
  Object.defineProperty(window, "Notification", {
    value: ctor,
    configurable: true,
    writable: true,
  })
  return { constructed }
}

function removeNotification() {
  Object.defineProperty(window, "Notification", {
    value: undefined,
    configurable: true,
    writable: true,
  })
}

describe("fork sendSystemNotification desktop payload", () => {
  beforeEach(() => {
    mocks.transportCall.mockReset()
    mocks.transportCall.mockResolvedValue(undefined)
    mocks.isDesktop.mockReturnValue(true)
    Object.defineProperty(document, "hidden", {
      configurable: true,
      get: () => true,
    })
  })

  it("sends camelCase actionId + conversationId for conversation targets", async () => {
    await sendSystemNotification("Title", "Body", {
      kind: "conversation",
      conversationId: 42,
    })
    expect(mocks.transportCall).toHaveBeenCalledWith("send_notification", {
      title: "Title",
      body: "Body",
      actionId: "test-action-uuid",
      conversationId: 42,
      dedupeKey: null,
    })
    const args = mocks.transportCall.mock.calls[0]![1] as Record<
      string,
      unknown
    >
    expect(args).not.toHaveProperty("action_id")
    expect(args).not.toHaveProperty("conversation_id")
    expect(args).not.toHaveProperty("dedupe_key")
  })

  it("forwards host dedupeKey for multi-window once-per-version gate", async () => {
    await sendSystemNotification(
      "Title",
      "Body",
      { kind: "conversation", conversationId: 9 },
      { dedupeKey: "lease-1:2" }
    )
    expect(mocks.transportCall).toHaveBeenCalledWith(
      "send_notification",
      expect.objectContaining({
        actionId: "test-action-uuid",
        conversationId: 9,
        dedupeKey: "lease-1:2",
      })
    )
  })

  it("omits actionId/conversationId when no target", async () => {
    await sendSystemNotification("Title", "Body")
    expect(mocks.transportCall).toHaveBeenCalledWith("send_notification", {
      title: "Title",
      body: "Body",
      actionId: null,
      conversationId: null,
      dedupeKey: null,
    })
  })

  it("does not invoke when document is visible", async () => {
    Object.defineProperty(document, "hidden", {
      configurable: true,
      get: () => false,
    })
    await sendSystemNotification("Title", "Body", {
      kind: "conversation",
      conversationId: 1,
    })
    expect(mocks.transportCall).not.toHaveBeenCalled()
  })
})

describe("upstream getNotificationPermission", () => {
  beforeEach(() => {
    mocks.shellCall.mockClear()
    mocks.remoteCall.mockClear()
    mocks.isDesktop.mockReturnValue(true)
    removeNotification()
  })

  it("reports the desktop as OS-managed rather than inventing a state", () => {
    // Neither notification backend exposes one: the Tauri plugin hard-codes
    // `Granted` on desktop and mac-notification-sys has no permission API at
    // all. Claiming "granted" here would tell a user with Codeg switched off
    // in System Settings that everything is fine.
    expect(getNotificationPermission()).toBe("managed_by_os")
  })

  it("reports `unsupported` when the browser has no Notification API", () => {
    // The shape of a `codeg-server` reached over plain http:// on a LAN
    // address: not a secure context, so the constructor is simply absent.
    mocks.isDesktop.mockReturnValue(false)
    expect(getNotificationPermission()).toBe("unsupported")
  })

  it.each([
    ["granted", "granted"],
    ["denied", "denied"],
    ["default", "default"],
    ["something-else", "default"],
  ])("maps browser permission %s to %s", (browser, expected) => {
    mocks.isDesktop.mockReturnValue(false)
    installNotification(browser)
    expect(getNotificationPermission()).toBe(expected)
  })
})

describe("upstream requestNotificationPermission", () => {
  beforeEach(() => {
    mocks.shellCall.mockClear()
    mocks.remoteCall.mockClear()
    mocks.isDesktop.mockReturnValue(true)
    removeNotification()
  })

  it("asks the browser and reports the answer", async () => {
    mocks.isDesktop.mockReturnValue(false)
    installNotification("default", "granted")

    await expect(requestNotificationPermission()).resolves.toBe("granted")
  })

  it("is a no-op on desktop", async () => {
    await expect(requestNotificationPermission()).resolves.toBe("managed_by_os")
  })

  it("treats a rejected request as still undecided", async () => {
    mocks.isDesktop.mockReturnValue(false)
    installNotification("default")
    const ctor = window.Notification as unknown as FakeNotificationCtor
    ctor.requestPermission = vi.fn(async () => {
      throw new Error("legacy callback API")
    })

    await expect(requestNotificationPermission()).resolves.toBe("default")
  })
})

describe("upstream deliverSystemNotification", () => {
  beforeEach(() => {
    mocks.shellCall.mockClear()
    mocks.shellCall.mockResolvedValue(undefined)
    mocks.transportCall.mockClear()
    mocks.isDesktop.mockReturnValue(true)
    removeNotification()
  })

  it("posts through the LOCAL shell transport, never the remote one", async () => {
    // Regression: this used `getTransport()`, which in a remote-desktop window
    // is the remote HTTP transport — and `send_notification` is a
    // `tauri-runtime`-only command the `codeg-server` binary never registers.
    // Every notification in those windows failed on the far end and was
    // swallowed by the caller's `.catch()`.
    await deliverSystemNotification("t", "b")

    expect(mocks.shellCall).toHaveBeenCalledWith("send_notification", {
      title: "t",
      body: "b",
    })
    expect(mocks.transportCall).not.toHaveBeenCalled()
  })

  it("propagates a backend failure instead of swallowing it", async () => {
    mocks.shellCall.mockRejectedValueOnce(new Error("could not deliver"))
    await expect(deliverSystemNotification("t", "b")).rejects.toThrow(
      "could not deliver"
    )
  })

  it("constructs a browser notification once permission is granted", async () => {
    mocks.isDesktop.mockReturnValue(false)
    const { constructed } = installNotification("granted")

    await deliverSystemNotification("t", "b")

    expect(constructed).toEqual([["t", { body: "b" }]])
  })

  it("refuses, rather than prompting, when the browser has not granted", async () => {
    // A prompt raised from the event path cannot succeed — the page is
    // backgrounded and carries no user activation. Requesting belongs to the
    // Settings button and nowhere else.
    mocks.isDesktop.mockReturnValue(false)
    const { constructed } = installNotification("default")
    const ctor = window.Notification as unknown as FakeNotificationCtor

    await expect(deliverSystemNotification("t", "b")).rejects.toThrow(
      /permission/i
    )
    expect(ctor.requestPermission).not.toHaveBeenCalled()
    expect(constructed).toEqual([])
  })

  it("fails loudly with no Notification API at all", async () => {
    mocks.isDesktop.mockReturnValue(false)
    await expect(deliverSystemNotification("t", "b")).rejects.toThrow(
      /not available/i
    )
  })
})

describe("upstream getNotificationIdentity", () => {
  beforeEach(() => {
    mocks.shellCall.mockClear()
    mocks.transportCall.mockClear()
    mocks.isDesktop.mockReturnValue(true)
    removeNotification()
  })

  it("reads the delivering identity over the LOCAL shell transport", async () => {
    // Same reasoning as delivery: in a remote-desktop window `getTransport()`
    // points at a host that never registers this command, and the identity we
    // want to report is the one on the screen in front of the user.
    mocks.shellCall.mockResolvedValueOnce({
      bundleId: "app.codeg",
      requestedBundleId: "app.codeg",
      degraded: false,
    })

    await expect(getNotificationIdentity()).resolves.toEqual({
      bundleId: "app.codeg",
      requestedBundleId: "app.codeg",
      degraded: false,
    })
    expect(mocks.shellCall).toHaveBeenCalledWith("notification_identity")
    expect(mocks.transportCall).not.toHaveBeenCalled()
  })

  it("reports a degraded identity verbatim", async () => {
    // The case this whole surface exists for: notifications posted under an
    // app the user never configured, while codeg's own switches govern
    // nothing.
    mocks.shellCall.mockResolvedValueOnce({
      bundleId: "com.apple.Terminal",
      requestedBundleId: "app.codeg",
      degraded: true,
    })

    await expect(getNotificationIdentity()).resolves.toMatchObject({
      bundleId: "com.apple.Terminal",
      degraded: true,
    })
  })

  it("has nothing to report in a browser", async () => {
    mocks.isDesktop.mockReturnValue(false)
    await expect(getNotificationIdentity()).resolves.toBeNull()
    expect(mocks.shellCall).not.toHaveBeenCalled()
  })

  it("passes through a desktop that declines to name an identity", async () => {
    // Windows and Linux: the backend answers `null` rather than claiming an
    // identity it cannot actually verify.
    mocks.shellCall.mockResolvedValueOnce(null)
    await expect(getNotificationIdentity()).resolves.toBeNull()
  })
})

describe("upstream openSystemNotificationSettings", () => {
  beforeEach(() => {
    mocks.shellCall.mockClear()
    mocks.isDesktop.mockReturnValue(true)
    removeNotification()
  })

  it("calls the local command on desktop", async () => {
    await openSystemNotificationSettings()
    expect(mocks.shellCall).toHaveBeenCalledWith(
      "open_system_notification_settings"
    )
  })

  it("refuses in a browser, where no page may open the permission UI", async () => {
    mocks.isDesktop.mockReturnValue(false)
    await expect(openSystemNotificationSettings()).rejects.toThrow(/desktop/i)
    expect(mocks.shellCall).not.toHaveBeenCalled()
  })
})
