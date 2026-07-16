import { describe, expect, it } from "vitest"
import {
  evaluateStreamingPerf,
  summarizeSamples,
} from "./streaming-perf-report"

describe("summarizeSamples", () => {
  it("uses nearest-rank percentiles", () => {
    expect(summarizeSamples([1, 2, 3, 4, 100])).toEqual({
      count: 5,
      p50: 3,
      p95: 100,
      max: 100,
    })
  })

  it("returns zeros for empty samples", () => {
    expect(summarizeSamples([])).toEqual({
      count: 0,
      p50: 0,
      p95: 0,
      max: 0,
    })
  })
})

describe("evaluateStreamingPerf", () => {
  it("evaluates the exact Windows contract", () => {
    const result = evaluateStreamingPerf({
      batchToPaintMs: [20, 40, 80],
      inputToPaintMs: [10, 20, 45],
      longTasksMs: [120, 199],
      visualUpdatesPerSecond: 35,
      integrityOk: true,
    })
    expect(result).toEqual({
      batchToPaint: true,
      inputToPaint: true,
      longTask: true,
      visualCadence: true,
      eventIntegrity: true,
      passed: true,
    })
  })

  it("fails each gate independently", () => {
    const result = evaluateStreamingPerf({
      batchToPaintMs: [20, 40, 120],
      inputToPaintMs: [10, 20, 55],
      longTasksMs: [120, 201],
      visualUpdatesPerSecond: 29,
      integrityOk: false,
    })
    expect(result).toEqual({
      batchToPaint: false,
      inputToPaint: false,
      longTask: false,
      visualCadence: false,
      eventIntegrity: false,
      passed: false,
    })
  })

  it("fails batch/input gates when sample series are empty", () => {
    const result = evaluateStreamingPerf({
      batchToPaintMs: [],
      inputToPaintMs: [],
      longTasksMs: [],
      visualUpdatesPerSecond: 35,
      integrityOk: true,
    })
    expect(result.batchToPaint).toBe(false)
    expect(result.inputToPaint).toBe(false)
    // No long tasks observed remains a pass (nothing blocked the main thread).
    expect(result.longTask).toBe(true)
    expect(result.passed).toBe(false)
  })

  it("maps frame-gap and drift fallbacks into the long-task gate", () => {
    const ok = evaluateStreamingPerf({
      batchToPaintMs: [20],
      inputToPaintMs: [10],
      longTasksMs: [],
      frameGapsMs: [16, 32],
      eventLoopDriftMs: [5],
      visualUpdatesPerSecond: 35,
      integrityOk: true,
    })
    expect(ok.longTask).toBe(true)

    const bad = evaluateStreamingPerf({
      batchToPaintMs: [20],
      inputToPaintMs: [10],
      longTasksMs: [],
      frameGapsMs: [240],
      eventLoopDriftMs: [180],
      visualUpdatesPerSecond: 35,
      integrityOk: true,
    })
    expect(bad.longTask).toBe(false)
    expect(bad.passed).toBe(false)
  })
})
