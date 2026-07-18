import { afterEach, describe, expect, it, vi } from "vitest"
import { scheduleIdleWork } from "./idle-work"

afterEach(() => {
  vi.unstubAllGlobals()
  vi.useRealTimers()
})

describe("scheduleIdleWork", () => {
  it("uses requestIdleCallback and cancels it", () => {
    const request = vi.fn(() => 7)
    const cancel = vi.fn()
    vi.stubGlobal("requestIdleCallback", request)
    vi.stubGlobal("cancelIdleCallback", cancel)
    const dispose = scheduleIdleWork(vi.fn(), { timeoutMs: 1_000 })
    dispose()
    expect(request).toHaveBeenCalledWith(expect.any(Function), {
      timeout: 1_000,
    })
    expect(cancel).toHaveBeenCalledWith(7)
  })

  it("falls back to a cancellable timeout on WKWebView", () => {
    vi.useFakeTimers()
    vi.stubGlobal("requestIdleCallback", undefined)
    const work = vi.fn()
    scheduleIdleWork(work, { timeoutMs: 50 })
    vi.advanceTimersByTime(49)
    expect(work).not.toHaveBeenCalled()
    vi.advanceTimersByTime(1)
    expect(work).toHaveBeenCalledTimes(1)
  })

  it("does not run work after dispose on the timeout fallback", () => {
    vi.useFakeTimers()
    vi.stubGlobal("requestIdleCallback", undefined)
    const work = vi.fn()
    const dispose = scheduleIdleWork(work, { timeoutMs: 50 })
    dispose()
    vi.advanceTimersByTime(50)
    expect(work).not.toHaveBeenCalled()
  })
})
