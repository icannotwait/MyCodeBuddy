import type { EventBusMetricsSnapshot } from "@/lib/types"

/** Windows WebView2 absolute gate: batch-receipt-to-paint P95. */
export const BATCH_TO_PAINT_P95_MS = 100
/** Windows WebView2 absolute gate: input-to-paint P95. */
export const INPUT_TO_PAINT_P95_MS = 50
/** Windows WebView2 absolute gate: no main-thread task above this. */
export const LONG_TASK_MAX_MS = 200
/** Windows WebView2 absolute gate: minimum visual updates while input queued. */
export const VISUAL_UPDATES_PER_SECOND_MIN = 30

export const GROK_RICH_V1_EXPECTED_EVENTS = 1223
export const GROK_RICH_V1_EXPECTED_TEXT_CHARS = 30_000
export const GROK_RICH_V1_EXPECTED_TEXT_SHA256 =
  "65380735c9a752758c7bace17cc722d86400480a0ae1dff62759f37eafa4b039"

export interface MetricSummary {
  count: number
  p50: number
  p95: number
  max: number
}

export interface StreamingPerformanceFlags {
  desktop_acp_event_batching: boolean
  incremental_live_transcript: boolean
  deferred_streaming_rich_content: boolean
}

export interface StreamingPerfAcceptance {
  batchToPaint: boolean
  inputToPaint: boolean
  longTask: boolean
  visualCadence: boolean
  eventIntegrity: boolean
  passed: boolean
}

export interface StreamingPerfReport {
  schemaVersion: 1
  runId: string
  fixture: {
    id: "grok_rich_v1"
    version: "grok-rich-v1"
    seed: number
    rateProfile: "eps_100" | "eps_500" | "eps_1000"
    expectedEvents: number
    expectedTextChars: 30000
    expectedTextSha256: string
  }
  environment: {
    platform: string
    userAgent: string
    webviewVersion: string | null
    buildMode: "development" | "production"
    hardwareAcceleration: "enabled" | "disabled" | "unknown"
    deliveryMode: "legacy" | "batched"
    flags: StreamingPerformanceFlags
  }
  delivery: EventBusMetricsSnapshot
  pipelineCounts: {
    deliveryCallbacks: number
    ingestorFrames: number
    connectionMapPublications: number
    connectionTransactions: number
    livePublications: number
    reactCommits: number
    paints: number
  }
  timings: {
    receiptToTransaction: MetricSummary
    transactionToLivePublication: MetricSummary
    batchToCommit: MetricSummary
    batchToPaint: MetricSummary
    inputToPaint: MetricSummary
    longTasks: MetricSummary
    frameGaps: MetricSummary
    eventLoopDrift: MetricSummary
  }
  renders: Record<
    | "conversationPanel"
    | "historicalThread"
    | "historicalRow"
    | "liveRow"
    | "markdownBlock"
    | "toolCard",
    number
  >
  integrity: {
    expectedEvents: number
    appliedEvents: number
    firstSeq: number
    lastSeq: number
    duplicateCount: number
    gapCount: number
    finalTextSha256: string
    ok: boolean
  }
  cadence: {
    queuedDurationMs: number
    paintCount: number
    updatesPerSecond: number
  }
  resources: {
    markdownCacheEntries: number | null
    markdownCacheBytes: number | null
    highlightCacheEntries: number | null
    highlightCacheBytes: number | null
    liveConversations: number | null
    liveSegments: number | null
    liveTools: number | null
    usedHeapBytes: number | null
    heapMeasurement: "supported" | "unsupported"
  }
  acceptance: StreamingPerfAcceptance
}

/**
 * Nearest-rank percentile: index = ceil(percentile * count) - 1.
 * Empty input yields a zero summary.
 */
export function summarizeSamples(samples: readonly number[]): MetricSummary {
  if (samples.length === 0) {
    return { count: 0, p50: 0, p95: 0, max: 0 }
  }
  const sorted = [...samples].sort((a, b) => a - b)
  const rank = (percentile: number) => {
    const index = Math.ceil(percentile * sorted.length) - 1
    return sorted[Math.min(Math.max(index, 0), sorted.length - 1)]
  }
  return {
    count: sorted.length,
    p50: rank(0.5),
    p95: rank(0.95),
    max: sorted[sorted.length - 1],
  }
}

export interface EvaluateStreamingPerfInput {
  batchToPaintMs: readonly number[]
  inputToPaintMs: readonly number[]
  longTasksMs: readonly number[]
  /** Used for long-task gate when `longTasksMs` is empty (no longtask observer). */
  frameGapsMs?: readonly number[]
  /** Used for long-task gate when `longTasksMs` is empty (timer-drift fallback). */
  eventLoopDriftMs?: readonly number[]
  visualUpdatesPerSecond: number
  integrityOk: boolean
}

/**
 * Prefer real longtask entries; otherwise map RAF gaps and timer drift into
 * long-task proxies so fallback-only environments still gate on main-thread jank.
 * Drift samples are `elapsed - 50`; convert back to wall delay as `50 + drift`.
 */
export function resolveLongTaskSamples(
  longTasksMs: readonly number[],
  frameGapsMs: readonly number[] = [],
  eventLoopDriftMs: readonly number[] = []
): number[] {
  if (longTasksMs.length > 0) {
    return [...longTasksMs]
  }
  const proxies: number[] = []
  for (const gap of frameGapsMs) {
    proxies.push(gap)
  }
  for (const drift of eventLoopDriftMs) {
    proxies.push(50 + drift)
  }
  return proxies
}

/** Apply the four Windows numeric gates plus event-integrity. */
export function evaluateStreamingPerf(
  input: EvaluateStreamingPerfInput
): StreamingPerfAcceptance {
  // Empty timing series must not pass via p95=0.
  const batchSummary = summarizeSamples(input.batchToPaintMs)
  const inputSummary = summarizeSamples(input.inputToPaintMs)
  const batchToPaint =
    batchSummary.count > 0 && batchSummary.p95 < BATCH_TO_PAINT_P95_MS
  const inputToPaint =
    inputSummary.count > 0 && inputSummary.p95 < INPUT_TO_PAINT_P95_MS
  const longTaskSamples = resolveLongTaskSamples(
    input.longTasksMs,
    input.frameGapsMs,
    input.eventLoopDriftMs
  )
  // No observed long tasks / proxies ⇒ pass; any sample must stay within max.
  const longTask =
    longTaskSamples.length === 0 ||
    summarizeSamples(longTaskSamples).max <= LONG_TASK_MAX_MS
  const visualCadence =
    input.visualUpdatesPerSecond >= VISUAL_UPDATES_PER_SECOND_MIN
  const eventIntegrity = input.integrityOk
  return {
    batchToPaint,
    inputToPaint,
    longTask,
    visualCadence,
    eventIntegrity,
    passed:
      batchToPaint &&
      inputToPaint &&
      longTask &&
      visualCadence &&
      eventIntegrity,
  }
}

export function emptyPipelineCounts(): StreamingPerfReport["pipelineCounts"] {
  return {
    deliveryCallbacks: 0,
    ingestorFrames: 0,
    connectionMapPublications: 0,
    connectionTransactions: 0,
    livePublications: 0,
    reactCommits: 0,
    paints: 0,
  }
}

export function emptyRenderCounts(): StreamingPerfReport["renders"] {
  return {
    conversationPanel: 0,
    historicalThread: 0,
    historicalRow: 0,
    liveRow: 0,
    markdownBlock: 0,
    toolCard: 0,
  }
}

export function legacyStreamingPerformanceFlags(): StreamingPerformanceFlags {
  return {
    desktop_acp_event_batching: false,
    incremental_live_transcript: false,
    deferred_streaming_rich_content: false,
  }
}

/** Extract WebView/Edge version from a user-agent string. */
export function extractWebviewVersion(userAgent: string): string | null {
  const edg = /Edg\/([\d.]+)/i.exec(userAgent)
  if (edg) return edg[1]
  const version = /Version\/([\d.]+)/i.exec(userAgent)
  if (version) return version[1]
  return null
}

/** Download a content-free JSON report via Blob URL (local only). */
export function downloadStreamingPerfReport(report: StreamingPerfReport): void {
  if (typeof document === "undefined") return
  const json = JSON.stringify(report, null, 2)
  const blob = new Blob([json], { type: "application/json" })
  const url = URL.createObjectURL(blob)
  const anchor = document.createElement("a")
  anchor.href = url
  anchor.download = `streaming-perf-${report.fixture.rateProfile}-${report.runId}.json`
  anchor.rel = "noopener"
  document.body.appendChild(anchor)
  anchor.click()
  anchor.remove()
  URL.revokeObjectURL(url)
}
