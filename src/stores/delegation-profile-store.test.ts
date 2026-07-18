import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

const mocks = vi.hoisted(() => ({
  getDelegationProfileCatalog: vi.fn(),
  subscribe: vi.fn(async () => () => {}),
  onReconnect: vi.fn(() => () => {}),
}))

vi.mock("@/lib/api", () => ({
  getDelegationProfileCatalog: mocks.getDelegationProfileCatalog,
}))

vi.mock("@/lib/platform", () => ({
  subscribe: mocks.subscribe,
  onTransportReconnect: mocks.onReconnect,
}))

import {
  resetDelegationProfileStore,
  useDelegationProfileStore,
} from "@/stores/delegation-profile-store"

beforeEach(() => {
  resetDelegationProfileStore()
  mocks.getDelegationProfileCatalog.mockReset()
  mocks.getDelegationProfileCatalog.mockResolvedValue({
    profiles: [],
    delegation_enabled: true,
    revision: 1,
  })
  mocks.subscribe.mockReset()
  mocks.subscribe.mockResolvedValue(() => {})
  mocks.onReconnect.mockReset()
  mocks.onReconnect.mockReturnValue(() => {})
})

afterEach(() => {
  resetDelegationProfileStore()
})

describe("useDelegationProfileStore", () => {
  it("initializes once and drops stale catalog events", async () => {
    await useDelegationProfileStore.getState().initialize()
    mocks.getDelegationProfileCatalog.mockClear()
    useDelegationProfileStore.getState().applyCatalog({
      profiles: [],
      delegation_enabled: false,
      revision: 0,
    })
    await useDelegationProfileStore.getState().initialize()
    expect(mocks.getDelegationProfileCatalog).not.toHaveBeenCalled()
    expect(useDelegationProfileStore.getState().catalog?.revision).toBe(1)
  })

  it("subscribes before bootstrap fetch and applies mid-bootstrap catalog events", async () => {
    let resolveBootstrap:
      | ((catalog: {
          profiles: []
          delegation_enabled: boolean
          revision: number
        }) => void)
      | undefined
    mocks.getDelegationProfileCatalog.mockImplementation(
      () =>
        new Promise((resolve) => {
          resolveBootstrap = resolve
        })
    )

    let eventHandler:
      | ((catalog: {
          profiles: []
          delegation_enabled: boolean
          revision: number
        }) => void)
      | undefined
    mocks.subscribe.mockImplementation(async (_event, handler) => {
      eventHandler = handler as typeof eventHandler
      return () => {}
    })

    const initPromise = useDelegationProfileStore.getState().initialize()

    await vi.waitFor(() => {
      expect(mocks.subscribe).toHaveBeenCalled()
      expect(eventHandler).toBeTypeOf("function")
    })
    expect(mocks.onReconnect).toHaveBeenCalled()

    // Emit a catalog event while bootstrap is still pending.
    eventHandler!({
      profiles: [],
      delegation_enabled: true,
      revision: 3,
    })
    expect(useDelegationProfileStore.getState().catalog?.revision).toBe(3)

    // Bootstrap returns a lower revision — revision gate must keep rev 3.
    resolveBootstrap!({
      profiles: [],
      delegation_enabled: false,
      revision: 1,
    })
    await initPromise

    expect(useDelegationProfileStore.getState().catalog?.revision).toBe(3)
    expect(useDelegationProfileStore.getState().ready).toBe(true)
    expect(useDelegationProfileStore.getState().error).toBeNull()
  })

  it("failed_bootstrap_is_ready_with_error_and_focus_refresh_recovers", async () => {
    mocks.getDelegationProfileCatalog.mockRejectedValueOnce(
      new Error("bootstrap boom")
    )

    const focusHandlers: Array<() => void> = []
    const addSpy = vi
      .spyOn(window, "addEventListener")
      .mockImplementation((type, listener) => {
        if (type === "focus" && typeof listener === "function") {
          focusHandlers.push(listener as () => void)
        }
      })

    await useDelegationProfileStore.getState().initialize()
    await vi.waitFor(() => {
      expect(useDelegationProfileStore.getState().ready).toBe(true)
    })
    expect(useDelegationProfileStore.getState().error).toBeTruthy()
    expect(useDelegationProfileStore.getState().catalog).toBeNull()

    mocks.getDelegationProfileCatalog.mockResolvedValue({
      profiles: [],
      delegation_enabled: true,
      revision: 2,
    })
    const focusCb = focusHandlers[0]
    expect(focusCb).toBeTypeOf("function")
    await focusCb()

    await vi.waitFor(() => {
      expect(useDelegationProfileStore.getState().catalog?.revision).toBe(2)
    })
    expect(useDelegationProfileStore.getState().error).toBeNull()
    expect(useDelegationProfileStore.getState().ready).toBe(true)

    addSpy.mockRestore()
  })

  it("successful_equal_revision_refresh_clears_a_transient_error_without_replacing_catalog", async () => {
    useDelegationProfileStore.getState().applyCatalog({
      profiles: [
        {
          id: "11111111-1111-4111-8111-111111111111",
          agent_type: "codebuddy",
          name: "A",
          config_values: {},
          enabled: true,
          created_at: 1,
          updated_at: 1,
        },
      ],
      delegation_enabled: true,
      revision: 2,
    })
    const seeded = useDelegationProfileStore.getState().catalog

    mocks.getDelegationProfileCatalog.mockRejectedValueOnce(
      new Error("transient")
    )
    await useDelegationProfileStore.getState().refresh()
    expect(useDelegationProfileStore.getState().error).toBeTruthy()
    expect(useDelegationProfileStore.getState().catalog).toEqual(seeded)

    mocks.getDelegationProfileCatalog.mockResolvedValue({
      profiles: [],
      delegation_enabled: false,
      revision: 2,
    })
    await useDelegationProfileStore.getState().refresh()
    expect(useDelegationProfileStore.getState().error).toBeNull()
    // Equal revision must not replace the catalog, but must clear error.
    expect(useDelegationProfileStore.getState().catalog).toEqual(seeded)
  })
})
