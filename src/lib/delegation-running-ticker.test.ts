import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import {
  getRunningTickerVersion,
  retainRunningTicker,
  subscribeRunningTicker,
} from "@/lib/delegation-running-ticker"

describe("delegation-running-ticker", () => {
  let setIntervalSpy: ReturnType<typeof vi.spyOn>
  let clearIntervalSpy: ReturnType<typeof vi.spyOn>
  const releases: Array<() => void> = []
  const unsubs: Array<() => void> = []

  beforeEach(() => {
    vi.useFakeTimers()
    setIntervalSpy = vi.spyOn(globalThis, "setInterval")
    clearIntervalSpy = vi.spyOn(globalThis, "clearInterval")
  })

  afterEach(() => {
    // Drain any leftover interest / listeners so module state is clean for
    // the next test (module-level singleton).
    while (releases.length > 0) {
      releases.pop()?.()
    }
    while (unsubs.length > 0) {
      unsubs.pop()?.()
    }
    setIntervalSpy.mockRestore()
    clearIntervalSpy.mockRestore()
    vi.useRealTimers()
  })

  function retain(): () => void {
    const release = retainRunningTicker()
    releases.push(release)
    return release
  }

  function subscribe(cb: () => void): () => void {
    const unsub = subscribeRunningTicker(cb)
    unsubs.push(unsub)
    return unsub
  }

  it("does not start an interval with zero retainers", () => {
    const cb = vi.fn()
    subscribe(cb)
    const versionBefore = getRunningTickerVersion()
    expect(setIntervalSpy).not.toHaveBeenCalled()
    vi.advanceTimersByTime(5_000)
    expect(cb).not.toHaveBeenCalled()
    expect(getRunningTickerVersion()).toBe(versionBefore)
  })

  it("starts a single 1s interval on the first retain", () => {
    retain()
    expect(setIntervalSpy).toHaveBeenCalledTimes(1)
    expect(setIntervalSpy).toHaveBeenCalledWith(expect.any(Function), 1_000)
  })

  it("does not open a second interval for additional retains", () => {
    retain()
    retain()
    retain()
    expect(setIntervalSpy).toHaveBeenCalledTimes(1)
  })

  it("bumps version and notifies all subscribers on each tick", () => {
    const a = vi.fn()
    const b = vi.fn()
    subscribe(a)
    subscribe(b)
    retain()

    const v0 = getRunningTickerVersion()
    vi.advanceTimersByTime(1_000)
    expect(getRunningTickerVersion()).toBe(v0 + 1)
    expect(a).toHaveBeenCalledTimes(1)
    expect(b).toHaveBeenCalledTimes(1)

    vi.advanceTimersByTime(1_000)
    expect(getRunningTickerVersion()).toBe(v0 + 2)
    expect(a).toHaveBeenCalledTimes(2)
    expect(b).toHaveBeenCalledTimes(2)
  })

  it("keeps the interval alive until the last retainer releases", () => {
    const releaseA = retain()
    const releaseB = retain()
    const cb = vi.fn()
    subscribe(cb)

    releaseA()
    expect(clearIntervalSpy).not.toHaveBeenCalled()
    vi.advanceTimersByTime(1_000)
    expect(cb).toHaveBeenCalledTimes(1)

    releaseB()
    expect(clearIntervalSpy).toHaveBeenCalledTimes(1)

    const callsBefore = cb.mock.calls.length
    const versionBefore = getRunningTickerVersion()
    vi.advanceTimersByTime(5_000)
    expect(cb).toHaveBeenCalledTimes(callsBefore)
    expect(getRunningTickerVersion()).toBe(versionBefore)
  })

  it("stops the interval when the sole retainer releases", () => {
    const release = retain()
    expect(setIntervalSpy).toHaveBeenCalledTimes(1)
    release()
    expect(clearIntervalSpy).toHaveBeenCalledTimes(1)
  })

  it("is StrictMode-safe under double retain/release (remount)", () => {
    // React StrictMode: mount → unmount → remount. Two overlapping retains
    // during the double-invoke phase must not open two intervals, and a
    // clean remount after full release must start a fresh one.
    const release1 = retain()
    const release2 = retain() // double-mount / overlapping effect
    expect(setIntervalSpy).toHaveBeenCalledTimes(1)

    release1() // first effect cleanup
    expect(clearIntervalSpy).not.toHaveBeenCalled()

    release2() // second effect cleanup → last interest drops
    expect(clearIntervalSpy).toHaveBeenCalledTimes(1)

    // Remount after full unmount restarts exactly one interval.
    retain()
    expect(setIntervalSpy).toHaveBeenCalledTimes(2)

    const cb = vi.fn()
    subscribe(cb)
    vi.advanceTimersByTime(1_000)
    expect(cb).toHaveBeenCalledTimes(1)
  })

  it("treats release as idempotent (double cleanup cannot under-count)", () => {
    const release = retain()
    retain() // second independent interest
    expect(setIntervalSpy).toHaveBeenCalledTimes(1)

    release()
    release() // accidental double call — must not clear while peer is held
    expect(clearIntervalSpy).not.toHaveBeenCalled()

    vi.advanceTimersByTime(1_000)
    expect(getRunningTickerVersion()).toBeGreaterThan(0)
  })

  it("unsubscribes without affecting the interval lifecycle", () => {
    retain()
    const cb = vi.fn()
    const unsub = subscribe(cb)
    vi.advanceTimersByTime(1_000)
    expect(cb).toHaveBeenCalledTimes(1)

    unsub()
    vi.advanceTimersByTime(1_000)
    expect(cb).toHaveBeenCalledTimes(1)
    // Interval still owned by retain; not stopped by unsub alone.
    expect(clearIntervalSpy).not.toHaveBeenCalled()
  })

  it("restarts cleanly after a full stop", () => {
    const release = retain()
    const v0 = getRunningTickerVersion()
    vi.advanceTimersByTime(1_000)
    expect(getRunningTickerVersion()).toBe(v0 + 1)
    release()

    retain()
    expect(setIntervalSpy).toHaveBeenCalledTimes(2)
    const cb = vi.fn()
    subscribe(cb)
    vi.advanceTimersByTime(1_000)
    expect(cb).toHaveBeenCalledTimes(1)
    expect(getRunningTickerVersion()).toBe(v0 + 2)
  })
})
