import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import {
  __resetStreamingPerformanceConfigForTests,
  getStreamingPerformanceCacheStats,
  initializeStreamingPerformanceConfig,
  installStreamingPerformanceMemoryPressureHandler,
  resetStreamingPerformanceCaches,
} from "./streaming-performance-config"

/**
 * Recovery matrix (Task 15) — failure modes → coverage locations.
 *
 * | Failure mode                      | Recovery / test ownership                              |
 * | --------------------------------- | ------------------------------------------------------ |
 * | Seq-gap pause / resume            | acp-connections-context + event-ingestor tests         |
 * | Projector rebuild after throw     | live-transcript-store.test (projector boom rebuild)    |
 * | Invalid MD partition              | incremental-stream-blocks tests                       |
 * | Shiki raw fallback                | streamdown-plugins / code-block tests                  |
 * | Idle-callback highlight deferral  | streamdown-plugins.test                                |
 * | IntersectionObserver Mermaid      | prior mermaid / rich-engine suites                     |
 * | PerformanceObserver longtask      | prior longtask suites                                  |
 * | Memory pressure cache drop        | this file (install / fire / cleanup)                   |
 * | Backend-scoped cache reset        | live-transcript-store reset path                       |
 *
 * Synthetic multi-turn soak: live-transcript-store / weighted-lru.
 * Full GUI soak + Step 7 overscan re-measure deferred to Task 16
 * (comparison.md documents keep-800 decision).
 */

const ALL_FLAGS_TRUE = {
  mode: "batched" as const,
  perf_replay_available: true,
  failure_event: "streaming-performance-failure" as const,
  flags: {
    desktop_acp_event_batching: true,
    incremental_live_transcript: true,
    deferred_streaming_rich_content: true,
  },
}

describe("installStreamingPerformanceMemoryPressureHandler", () => {
  beforeEach(() => {
    __resetStreamingPerformanceConfigForTests()
  })

  afterEach(() => {
    __resetStreamingPerformanceConfigForTests()
    vi.restoreAllMocks()
    delete (window as { onmemorypressure?: unknown }).onmemorypressure
  })

  it("returns a safe disposer when onmemorypressure capability is absent", () => {
    delete (window as { onmemorypressure?: unknown }).onmemorypressure
    const dispose = installStreamingPerformanceMemoryPressureHandler()
    expect(typeof dispose).toBe("function")
    expect(() => dispose()).not.toThrow()
  })

  it("installs listener, handles memorypressure, and removes on cleanup", () => {
    Object.defineProperty(window, "onmemorypressure", {
      configurable: true,
      value: null,
      writable: true,
    })

    const addSpy = vi.spyOn(window, "addEventListener")
    const removeSpy = vi.spyOn(window, "removeEventListener")

    const dispose = installStreamingPerformanceMemoryPressureHandler()

    const memoryCalls = addSpy.mock.calls.filter(
      ([type]) => type === "memorypressure"
    )
    expect(memoryCalls).toHaveLength(1)
    const handler = memoryCalls[0][1] as EventListener

    // Synthetic pressure invokes resetStreamingPerformanceCaches (no-throw).
    expect(() => handler(new Event("memorypressure"))).not.toThrow()
    expect(getStreamingPerformanceCacheStats()).toEqual({
      markdownEntries: 0,
      markdownBytes: 0,
      highlightEntries: 0,
      highlightBytes: 0,
    })

    dispose()
    expect(
      removeSpy.mock.calls.some(([type]) => type === "memorypressure")
    ).toBe(true)

    // After cleanup, reinstall is allowed (idempotent path after nulling).
    const dispose2 = installStreamingPerformanceMemoryPressureHandler()
    dispose2()
  })

  it("idempotent install returns the same disposer without double-binding", () => {
    Object.defineProperty(window, "onmemorypressure", {
      configurable: true,
      value: null,
      writable: true,
    })
    const addSpy = vi.spyOn(window, "addEventListener")

    const d1 = installStreamingPerformanceMemoryPressureHandler()
    const d2 = installStreamingPerformanceMemoryPressureHandler()
    expect(d1).toBe(d2)

    const installs = addSpy.mock.calls.filter(
      ([type]) => type === "memorypressure"
    )
    expect(installs).toHaveLength(1)
    d1()
  })

  it("initializeStreamingPerformanceConfig installs pressure handler once", () => {
    Object.defineProperty(window, "onmemorypressure", {
      configurable: true,
      value: null,
      writable: true,
    })
    const addSpy = vi.spyOn(window, "addEventListener")

    initializeStreamingPerformanceConfig(ALL_FLAGS_TRUE)
    // Equal re-init is a no-op and must not throw.
    initializeStreamingPerformanceConfig(ALL_FLAGS_TRUE)

    const installs = addSpy.mock.calls.filter(
      ([type]) => type === "memorypressure"
    )
    expect(installs).toHaveLength(1)
  })

  it("resetStreamingPerformanceCaches is safe when caches are empty", () => {
    expect(() => resetStreamingPerformanceCaches()).not.toThrow()
    expect(getStreamingPerformanceCacheStats()).toEqual({
      markdownEntries: 0,
      markdownBytes: 0,
      highlightEntries: 0,
      highlightBytes: 0,
    })
  })
})
