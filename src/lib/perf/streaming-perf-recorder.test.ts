import { afterEach, describe, expect, it, vi } from "vitest"
import {
  StreamingPerfRecorder,
  probeInputToPaint,
  type PerfRunMetadata,
} from "./streaming-perf-recorder"

const runMetadata: PerfRunMetadata = {
  runId: "test-run",
  seed: 1,
  rateProfile: "eps_100",
}

function manualClock() {
  let value = 0
  return {
    now: () => value,
    set: (next: number) => {
      value = next
    },
    advance: (delta: number) => {
      value += delta
    },
  }
}

describe("StreamingPerfRecorder", () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it("matches a delivery to its commit and next paint", () => {
    const clock = manualClock()
    let pendingPaintRaf: FrameRequestCallback | null = null
    const deferredRaf = (callback: FrameRequestCallback): number => {
      pendingPaintRaf = callback
      return 1
    }
    const MockObserver = class {
      observe(): void {}
      disconnect(): void {}
    } as unknown as typeof PerformanceObserver
    const recorder = new StreamingPerfRecorder({
      clock,
      raf: deferredRaf,
      // Avoid the RAF fallback loop so controlled raf is only for paint flush.
      supportedEntryTypes: ["longtask"],
      performanceObserver: MockObserver,
    })
    recorder.start(runMetadata)
    clock.set(0)
    recorder.markBatchReceived(7, 4)
    clock.set(4)
    recorder.markTransactionComplete([7])
    clock.set(7)
    recorder.markLivePublication([7])
    clock.set(9)
    const committed = recorder.markReactCommit()
    expect(committed).toEqual([7])
    clock.set(12)
    pendingPaintRaf?.(12)
    expect(recorder.snapshot().batchToPaintMs).toEqual([12])
  })

  it("records paint samples across multi-commit before paint", () => {
    // Simulates rapid React re-renders: multiple commits drain pending IDs
    // before the browser paints; coalesced RAF must still flush all of them.
    const clock = manualClock()
    let pendingPaintRaf: FrameRequestCallback | null = null
    let rafSchedules = 0
    const deferredRaf = (callback: FrameRequestCallback): number => {
      pendingPaintRaf = callback
      rafSchedules += 1
      return rafSchedules
    }
    const MockObserver = class {
      observe(): void {}
      disconnect(): void {}
    } as unknown as typeof PerformanceObserver
    const recorder = new StreamingPerfRecorder({
      clock,
      raf: deferredRaf,
      supportedEntryTypes: ["longtask"],
      performanceObserver: MockObserver,
    })
    recorder.start(runMetadata)

    clock.set(0)
    recorder.markBatchReceived(1, 1)
    clock.set(1)
    recorder.markTransactionComplete([1])
    clock.set(2)
    recorder.markLivePublication([1])
    clock.set(3)
    recorder.markReactCommit() // drains [1], schedules paint RAF

    clock.set(4)
    recorder.markBatchReceived(2, 1)
    clock.set(5)
    recorder.markTransactionComplete([2])
    clock.set(6)
    recorder.markLivePublication([2])
    clock.set(7)
    recorder.markReactCommit() // drains [2], coalesces into same paint RAF

    // Only one paint RAF should be pending for both commits.
    expect(rafSchedules).toBe(1)
    expect(recorder.snapshot().batchToPaintMs).toEqual([])

    clock.set(10)
    pendingPaintRaf?.(10)
    // delivery 1 received at 0 → 10ms; delivery 2 received at 4 → 6ms
    expect(recorder.snapshot().batchToPaintMs).toEqual([10, 6])
    expect(recorder.snapshot().pipelineCounts.paints).toBe(1)
    expect(recorder.snapshot().pipelineCounts.reactCommits).toBe(2)
  })

  it("uses frame gaps and timer drift when longtask is unsupported", () => {
    const clock = manualClock()
    let pendingRaf: FrameRequestCallback | null = null
    const controlledRaf = (callback: FrameRequestCallback): number => {
      pendingRaf = callback
      return 1
    }
    function advanceRafBy(timestamp: number): void {
      const callback = pendingRaf
      pendingRaf = null
      callback?.(timestamp)
    }

    let pendingTimer: { callback: () => void; delay: number } | null = null
    const controlledTimer = (callback: () => void, delay: number): number => {
      pendingTimer = { callback, delay }
      return 1
    }
    function advanceTimerBy(delta: number): void {
      clock.advance(delta)
      const timer = pendingTimer
      pendingTimer = null
      timer?.callback()
    }

    const recorder = new StreamingPerfRecorder({
      supportedEntryTypes: [],
      clock,
      raf: controlledRaf,
      setTimer: controlledTimer,
      clearTimer: () => {
        pendingTimer = null
      },
    })
    recorder.start(runMetadata)
    advanceRafBy(240)
    advanceTimerBy(230)
    expect(recorder.snapshot().frameGapsMs).toContain(240)
    expect(recorder.snapshot().eventLoopDriftMs).toContain(180)
  })

  it("waitForQuiet completes while fallback long-task loops keep sampling", async () => {
    const clock = manualClock()
    const rafCallbacks: FrameRequestCallback[] = []
    const controlledRaf = (callback: FrameRequestCallback): number => {
      rafCallbacks.push(callback)
      return rafCallbacks.length
    }

    type PendingTimer = { callback: () => void; delay: number; id: number }
    const timers: PendingTimer[] = []
    let nextTimerId = 1
    const controlledTimer = (callback: () => void, delay: number): number => {
      const id = nextTimerId++
      timers.push({ callback, delay, id })
      return id
    }
    const clearTimer = (handle: number) => {
      const idx = timers.findIndex((t) => t.id === handle)
      if (idx >= 0) timers.splice(idx, 1)
    }
    function fireDueTimers(maxDelay: number): void {
      // Fire any scheduled timers with delay <= maxDelay once each (FIFO).
      const due = timers.filter((t) => t.delay <= maxDelay)
      for (const t of due) {
        clearTimer(t.id)
        t.callback()
      }
    }

    const recorder = new StreamingPerfRecorder({
      supportedEntryTypes: [],
      clock,
      raf: controlledRaf,
      setTimer: controlledTimer,
      clearTimer,
    })
    recorder.start(runMetadata)
    // Fallback observers are running; advance activity then only clock + polls.
    clock.set(100)
    recorder.markBatchReceived(1, 1)
    // lastActivityAt = 100. Quiet needs now - lastActivity >= 250.
    // Fallback raf/drift must not refresh lastActivityAt.
    clock.set(100)
    // Drive a few fallback samples during the quiet wait.
    const quietPromise = recorder.waitForQuiet(250, 5_000)

    // Simulate poll + drift loops without pipeline activity.
    for (let i = 0; i < 20; i++) {
      clock.advance(25)
      // Fire fallback raf frame (gap sampling)
      const frame = rafCallbacks[rafCallbacks.length - 1]
      frame?.(clock.now())
      fireDueTimers(50)
    }
    await expect(quietPromise).resolves.toBeUndefined()
    expect(recorder.snapshot().frameGapsMs.length).toBeGreaterThan(0)
  })

  it("waitForQuiet completes while input probes keep sampling", async () => {
    const clock = manualClock()
    type PendingTimer = { callback: () => void; delay: number; id: number }
    const timers: PendingTimer[] = []
    let nextTimerId = 1
    const setTimer = (callback: () => void, delay: number): number => {
      const id = nextTimerId++
      timers.push({ callback, delay, id })
      return id
    }
    const clearTimer = (handle: number) => {
      const idx = timers.findIndex((t) => t.id === handle)
      if (idx >= 0) timers.splice(idx, 1)
    }
    function fireDueTimers(maxDelay: number): void {
      const due = timers.filter((t) => t.delay <= maxDelay)
      for (const t of due) {
        clearTimer(t.id)
        t.callback()
      }
    }

    const recorder = new StreamingPerfRecorder({
      clock,
      setTimer,
      clearTimer,
      supportedEntryTypes: ["longtask"],
      performanceObserver: class {
        observe(): void {}
        disconnect(): void {}
      } as unknown as typeof PerformanceObserver,
    })
    recorder.start(runMetadata)
    // Pipeline activity at t=100; quiet needs lastActivity + 250 with no touch.
    clock.set(100)
    recorder.markBatchReceived(1, 1)

    const quietPromise = recorder.waitForQuiet(250, 5_000)

    // Simulate MessageInput's 100ms probe interval during quiet wait.
    for (let i = 0; i < 10; i++) {
      clock.advance(50)
      const probeId = recorder.markInputQueued()
      clock.advance(1)
      recorder.markInputPaint(probeId)
      fireDueTimers(50)
    }

    await expect(quietPromise).resolves.toBeUndefined()
    expect(recorder.snapshot().inputToPaintMs.length).toBeGreaterThan(0)
  })

  it("freezes cadence duration so quiet drain does not inflate UPS window", async () => {
    const clock = manualClock()
    let pendingPaintRaf: FrameRequestCallback | null = null
    const deferredRaf = (callback: FrameRequestCallback): number => {
      pendingPaintRaf = callback
      return 1
    }
    const MockObserver = class {
      observe(): void {}
      disconnect(): void {}
    } as unknown as typeof PerformanceObserver

    type PendingTimer = { callback: () => void; id: number }
    const timers: PendingTimer[] = []
    let nextId = 1
    const setTimer = (callback: () => void, _delay: number): number => {
      const id = nextId++
      timers.push({ callback, id })
      return id
    }
    const clearTimer = (handle: number) => {
      const idx = timers.findIndex((t) => t.id === handle)
      if (idx >= 0) timers.splice(idx, 1)
    }

    const recorder = new StreamingPerfRecorder({
      clock,
      raf: deferredRaf,
      setTimer,
      clearTimer,
      supportedEntryTypes: ["longtask"],
      performanceObserver: MockObserver,
    })
    recorder.start(runMetadata)
    clock.set(0)
    recorder.markInputQueued()
    clock.set(0)
    recorder.markBatchReceived(1, 1)
    clock.set(5)
    recorder.markTransactionComplete([1])
    clock.set(10)
    recorder.markLivePublication([1])
    clock.set(15)
    recorder.markReactCommit()
    clock.set(20)
    pendingPaintRaf?.(20)

    // Start quiet wait at t=20; freeze should use lastActivity (~20), not t after drain.
    const quiet = recorder.waitForQuiet(250, 5_000)
    clock.set(500)
    while (timers.length > 0) {
      const t = timers.shift()
      t?.callback()
    }
    await quiet

    const report = recorder.buildReport()
    expect(report.cadence.queuedDurationMs).toBeLessThanOrEqual(20)
    expect(report.cadence.queuedDurationMs).toBeGreaterThan(0)
  })

  it("does no allocation when inactive", () => {
    const recorder = new StreamingPerfRecorder()
    recorder.markBatchReceived(1, 1)
    expect(recorder.isActive()).toBe(false)
    expect(recorder.debugAllocationCount()).toBe(0)
  })

  it("records input-to-paint samples via MessageChannel probe", () => {
    const clock = manualClock()
    const rafQueue: FrameRequestCallback[] = []
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
      rafQueue.push(cb)
      return rafQueue.length
    })
    const MockObserver = class {
      observe(): void {}
      disconnect(): void {}
    } as unknown as typeof PerformanceObserver

    class FakeMessageChannel {
      port1 = {
        onmessage: null as ((ev: MessageEvent) => void) | null,
        close: vi.fn(),
      }
      port2 = {
        postMessage: () => {
          this.port1.onmessage?.(new MessageEvent("message"))
        },
        close: vi.fn(),
      }
    }
    vi.stubGlobal("MessageChannel", FakeMessageChannel)

    const recorder = new StreamingPerfRecorder({
      clock,
      supportedEntryTypes: ["longtask"],
      performanceObserver: MockObserver,
    })
    recorder.start(runMetadata)
    clock.set(10)
    probeInputToPaint(recorder)
    clock.set(25)
    while (rafQueue.length > 0) {
      const cb = rafQueue.shift()
      cb?.(25)
    }
    expect(recorder.snapshot().inputToPaintMs).toEqual([15])
  })
})
