import { act, renderHook, waitFor } from "@testing-library/react"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import { useDelegateAccess } from "./use-delegate-access"
import { isDelegateViewerOnlyRejection } from "@/lib/delegate-access"
import { subscribe } from "@/lib/platform"

const h = vi.hoisted(() => ({
  get: vi.fn(),
  handlers: new Map<string, (payload: unknown) => void>(),
  reconnect: null as (() => void) | null,
}))

vi.mock("@/lib/api", () => ({ getDelegateAccess: h.get }))
vi.mock("@/lib/platform", () => ({
  subscribe: vi.fn(
    async (name: string, handler: (payload: unknown) => void) => {
      h.handlers.set(name, handler)
      return () => h.handlers.delete(name)
    }
  ),
  onTransportReconnect: (callback: () => void) => {
    h.reconnect = callback
    return () => {
      h.reconnect = null
    }
  },
}))

beforeEach(() => {
  h.get.mockReset()
  h.handlers.clear()
  h.reconnect = null
  vi.mocked(subscribe).mockImplementation(
    async (name: string, handler: (payload: unknown) => void) => {
      h.handlers.set(name, handler)
      return () => h.handlers.delete(name)
    }
  )
})

afterEach(() => vi.useRealTimers())

describe("useDelegateAccess", () => {
  it("is fail-closed while loading and on lookup failure", async () => {
    let reject!: (error: Error) => void
    h.get.mockReturnValue(
      new Promise((_, r) => {
        reject = r
      })
    )
    const { result } = renderHook(() =>
      useDelegateAccess({ conversationId: 7, enabled: true })
    )
    expect(result.current.access).toEqual({
      mode: "viewer_only",
      reason: "state_unknown",
      parent_id: null,
    })
    act(() => reject(new Error("offline")))
    await waitFor(() => expect(result.current.loading).toBe(false))
    expect(result.current.access.reason).toBe("state_unknown")
  })

  it("exposes fail-closed viewer_only while a refresh is in flight", async () => {
    let resolveRefresh!: (value: unknown) => void
    h.get
      .mockResolvedValueOnce({
        mode: "interactive",
        reason: null,
        parent_id: 3,
      })
      .mockReturnValueOnce(
        new Promise((resolve) => {
          resolveRefresh = resolve
        })
      )
    const { result } = renderHook(() =>
      useDelegateAccess({ conversationId: 7, enabled: true })
    )
    await waitFor(() => expect(result.current.access.mode).toBe("interactive"))
    expect(result.current.loading).toBe(false)

    act(() => {
      void result.current.refresh()
    })
    await waitFor(() => expect(result.current.loading).toBe(true))
    // Must not keep presenting interactive while revalidating after a
    // lock-relevant refresh (parent/child event, reconnect, or manual).
    expect(result.current.access).toEqual({
      mode: "viewer_only",
      reason: "state_unknown",
      parent_id: 3,
    })

    act(() =>
      resolveRefresh({
        mode: "viewer_only",
        reason: "parent_turn_active",
        parent_id: 3,
      })
    )
    await waitFor(() => expect(result.current.loading).toBe(false))
    expect(result.current.access).toEqual({
      mode: "viewer_only",
      reason: "parent_turn_active",
      parent_id: 3,
    })
  })

  it("retries a failed lookup with backoff and cancels timers on unmount", async () => {
    vi.useFakeTimers()
    h.get
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValue({ mode: "interactive", reason: null, parent_id: 3 })
    const { result, unmount } = renderHook(() =>
      useDelegateAccess({ conversationId: 7, enabled: true })
    )
    await act(async () => {
      await Promise.resolve()
    })
    expect(result.current.access.reason).toBe("state_unknown")
    expect(h.get).toHaveBeenCalledTimes(1)

    await act(async () => {
      await vi.advanceTimersByTimeAsync(300)
    })
    expect(h.get).toHaveBeenCalledTimes(2)
    expect(result.current.access.mode).toBe("interactive")

    h.get.mockRejectedValueOnce(new Error("offline again"))
    await act(async () => {
      await result.current.refresh()
    })
    unmount()
    await vi.runAllTimersAsync()
    expect(h.get).toHaveBeenCalledTimes(3)
  })

  it("never applies a stale interactive result after the child id changes", async () => {
    let resolveOld!: (value: unknown) => void
    h.get
      .mockReturnValueOnce(
        new Promise((resolve) => {
          resolveOld = resolve
        })
      )
      .mockResolvedValueOnce({
        mode: "viewer_only",
        reason: "task_running",
        parent_id: 4,
      })
    const { result, rerender } = renderHook(
      ({ id }) => useDelegateAccess({ conversationId: id, enabled: true }),
      { initialProps: { id: 7 } }
    )
    // Wait until the deferred post-subscribe load for child 7 is in flight so
    // the hanging promise is bound to the old scope, not the next child.
    await waitFor(() => expect(h.get).toHaveBeenCalledWith(7))
    rerender({ id: 8 })
    await waitFor(() => expect(h.get).toHaveBeenCalledWith(8))
    act(() => resolveOld({ mode: "interactive", reason: null, parent_id: 3 }))
    await waitFor(() =>
      expect(result.current.access.reason).toBe("task_running")
    )
  })

  it("coalesces a deferred refresh into a single follow-up request", async () => {
    let resolveFirst!: (value: unknown) => void
    h.get
      .mockReturnValueOnce(
        new Promise((resolve) => {
          resolveFirst = resolve
        })
      )
      .mockResolvedValue({
        mode: "viewer_only",
        reason: "task_running",
        parent_id: 3,
      })
    const { result } = renderHook(() =>
      useDelegateAccess({ conversationId: 7, enabled: true })
    )
    await waitFor(() => expect(h.get).toHaveBeenCalledTimes(1))

    let firstRefresh!: Promise<void>
    let secondRefresh!: Promise<void>
    act(() => {
      firstRefresh = result.current.refresh()
      secondRefresh = result.current.refresh()
    })
    // Both refreshes share the in-flight request; no extra GET yet.
    expect(h.get).toHaveBeenCalledTimes(1)

    act(() => resolveFirst({ mode: "interactive", reason: null, parent_id: 3 }))
    await act(async () => {
      await Promise.all([firstRefresh, secondRefresh])
    })
    // One coalesced follow-up after the deferred rerun flag.
    await waitFor(() => expect(h.get).toHaveBeenCalledTimes(2))
    await waitFor(() =>
      expect(result.current.access.reason).toBe("task_running")
    )
  })

  it("coalesces refreshes and refreshes for child, parent, and reconnect", async () => {
    h.get.mockResolvedValue({
      mode: "viewer_only",
      reason: "parent_turn_active",
      parent_id: 3,
    })
    const { result } = renderHook(() =>
      useDelegateAccess({ conversationId: 7, enabled: true })
    )
    await waitFor(() => expect(h.get).toHaveBeenCalledTimes(1))
    const changed = h.handlers.get("conversation://changed")!
    act(() => {
      changed({ kind: "state", patch: { id: 3 } })
      changed({ kind: "state", patch: { id: 7 } })
    })
    await waitFor(() =>
      expect(h.get.mock.calls.length).toBeGreaterThanOrEqual(2)
    )
    act(() => h.reconnect?.())
    await waitFor(() =>
      expect(h.get.mock.calls.length).toBeGreaterThanOrEqual(3)
    )
    await act(async () => {
      await Promise.all([result.current.refresh(), result.current.refresh()])
    })
  })

  it("defers the initial access read until conversation subscription is ready", async () => {
    let releaseSubscribe!: () => void
    vi.mocked(subscribe).mockImplementationOnce(
      (name: string, handler: (payload: unknown) => void) =>
        new Promise((resolve) => {
          releaseSubscribe = () => {
            h.handlers.set(name, handler)
            resolve(() => h.handlers.delete(name))
          }
        })
    )
    h.get.mockResolvedValue({
      mode: "interactive",
      reason: null,
      parent_id: 3,
    })

    const { result } = renderHook(() =>
      useDelegateAccess({ conversationId: 7, enabled: true })
    )

    // Allow effects/microtasks to run while subscribe is still pending.
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(h.get).not.toHaveBeenCalled()
    expect(result.current.loading).toBe(true)
    expect(result.current.access).toEqual({
      mode: "viewer_only",
      reason: "state_unknown",
      parent_id: null,
    })

    await act(async () => {
      releaseSubscribe()
    })
    await waitFor(() => expect(h.get).toHaveBeenCalledTimes(1))
    await waitFor(() => expect(result.current.access.mode).toBe("interactive"))
    expect(h.handlers.has("conversation://changed")).toBe(true)
  })

  it("does not fetch on pending-subscription refresh or reconnect until ready", async () => {
    let releaseSubscribe!: () => void
    vi.mocked(subscribe).mockImplementationOnce(
      (name: string, handler: (payload: unknown) => void) =>
        new Promise((resolve) => {
          releaseSubscribe = () => {
            h.handlers.set(name, handler)
            resolve(() => h.handlers.delete(name))
          }
        })
    )
    h.get.mockResolvedValue({
      mode: "interactive",
      reason: null,
      parent_id: 3,
    })

    const { result } = renderHook(() =>
      useDelegateAccess({ conversationId: 7, enabled: true })
    )

    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(h.get).not.toHaveBeenCalled()
    expect(h.reconnect).not.toBeNull()

    // Manual refresh and transport reconnect must queue — not fetch — while
    // the conversation subscription is still pending.
    await act(async () => {
      await result.current.refresh()
    })
    expect(h.get).not.toHaveBeenCalled()

    act(() => {
      h.reconnect?.()
    })
    expect(h.get).not.toHaveBeenCalled()

    await act(async () => {
      releaseSubscribe()
    })
    // One post-ready load covers the initial read and any queued refresh.
    await waitFor(() => expect(h.get).toHaveBeenCalledTimes(1))
    await waitFor(() => expect(result.current.access.mode).toBe("interactive"))
  })

  it("matches parent events using parent_id immediately after a successful fetch", async () => {
    // Resolve the first access snapshot, then fire a parent conversation
    // change in the same act turn — after run()'s synchronous parentIdRef
    // write, but before relying on a passive effect to mirror access state.
    // A lagging parent_id would drop this event and never issue a 2nd fetch.
    let resolveFirst!: (value: unknown) => void
    h.get
      .mockReturnValueOnce(
        new Promise((resolve) => {
          resolveFirst = resolve
        })
      )
      .mockResolvedValue({
        mode: "viewer_only",
        reason: "parent_turn_active",
        parent_id: 3,
      })

    const { result } = renderHook(() =>
      useDelegateAccess({ conversationId: 7, enabled: true })
    )
    await waitFor(() => expect(h.get).toHaveBeenCalledTimes(1))

    await act(async () => {
      resolveFirst({
        mode: "interactive",
        reason: null,
        parent_id: 3,
      })
      // Flush the getDelegateAccess fulfillment (parentIdRef write).
      await Promise.resolve()
      await Promise.resolve()
      const changed = h.handlers.get("conversation://changed")!
      changed({ kind: "state", patch: { id: 3 } })
      // Allow coalesced follow-up run to start.
      await Promise.resolve()
      await Promise.resolve()
    })

    await waitFor(() => expect(h.get).toHaveBeenCalledTimes(2))
    await waitFor(() =>
      expect(result.current.access).toEqual({
        mode: "viewer_only",
        reason: "parent_turn_active",
        parent_id: 3,
      })
    )
  })

  it("recognizes the structured backend rejection", () => {
    expect(
      isDelegateViewerOnlyRejection({
        code: "delegate_viewer_only",
        message: "Delegated conversation is read-only",
        detail: "task_running",
      })
    ).toBe(true)
  })
})
