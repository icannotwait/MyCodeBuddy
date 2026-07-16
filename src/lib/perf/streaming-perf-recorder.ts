import {
  emptyPipelineCounts,
  emptyRenderCounts,
  evaluateStreamingPerf,
  extractWebviewVersion,
  GROK_RICH_V1_EXPECTED_EVENTS,
  GROK_RICH_V1_EXPECTED_TEXT_CHARS,
  GROK_RICH_V1_EXPECTED_TEXT_SHA256,
  legacyStreamingPerformanceFlags,
  summarizeSamples,
  type StreamingPerfReport,
  type StreamingPerformanceFlags,
} from "./streaming-perf-report"
import type { EventBusMetricsSnapshot } from "@/lib/types"

export type PerfRenderKind =
  | "conversationPanel"
  | "historicalThread"
  | "historicalRow"
  | "liveRow"
  | "markdownBlock"
  | "toolCard"

export type PerfRateProfile = "eps_100" | "eps_500" | "eps_1000"

export interface PerfRunMetadata {
  runId?: string
  seed?: number
  rateProfile?: PerfRateProfile
  expectedEvents?: number
  expectedTextSha256?: string
}

export interface PerfClock {
  now: () => number
}

export type PerfRaf = (callback: FrameRequestCallback) => number
export type PerfCancelRaf = (handle: number) => void
export type PerfSetTimer = (callback: () => void, delayMs: number) => number
export type PerfClearTimer = (handle: number) => void

export interface StreamingPerfRecorderOptions {
  clock?: PerfClock
  raf?: PerfRaf
  cancelRaf?: PerfCancelRaf
  setTimer?: PerfSetTimer
  clearTimer?: PerfClearTimer
  supportedEntryTypes?: readonly string[]
  performanceObserver?: typeof PerformanceObserver | null
}

interface DeliveryRecord {
  receivedAt: number
  eventCount: number
  transactionAt?: number
  liveAt?: number
  commitAt?: number
  paintAt?: number
}

interface InputProbeRecord {
  queuedAt: number
  paintAt?: number
}

interface ActiveRun {
  metadata: Required<
    Pick<PerfRunMetadata, "runId" | "seed" | "rateProfile">
  > & {
    expectedEvents: number
    expectedTextSha256: string
  }
  startedAt: number
  lastActivityAt: number
  deliveries: Map<number, DeliveryRecord>
  pendingPaintIds: Set<number>
  inputProbes: Map<number, InputProbeRecord>
  nextInputProbeId: number
  pipelineCounts: StreamingPerfReport["pipelineCounts"]
  renderCounts: StreamingPerfReport["renders"]
  receiptToTransactionMs: number[]
  transactionToLivePublicationMs: number[]
  batchToCommitMs: number[]
  batchToPaintMs: number[]
  inputToPaintMs: number[]
  longTasksMs: number[]
  frameGapsMs: number[]
  eventLoopDriftMs: number[]
  allocationCount: number
  longTaskObserver: PerformanceObserver | null
  rafHandle: number | null
  driftTimerHandle: number | null
  lastRafTimestamp: number | null
  lastDriftScheduledAt: number | null
  cadenceQueuedStartedAt: number | null
  cadenceQueuedDurationMs: number
  cadencePaintCount: number
  /** Once true, cadence duration no longer extends (excludes quiet drain). */
  cadenceFrozen: boolean
  /**
   * Delivery IDs committed and waiting for a coalesced next-paint RAF.
   * Survives rapid React re-renders; not cancelled by effect cleanup.
   */
  paintFlushIds: number[]
  paintRafHandle: number | null
  integrity: StreamingPerfReport["integrity"]
  deliverySnapshot: EventBusMetricsSnapshot | null
  environment: StreamingPerfReport["environment"] | null
}

declare global {
  interface Window {
    __codegStreamingPerf?: {
      run(options: {
        rateProfile: "eps_100" | "eps_500" | "eps_1000"
        seed?: number
        download?: boolean
      }): Promise<StreamingPerfReport>
      debugState?: () => {
        activeKey: string | null
        connections: Array<{
          key: string
          connectionId: string | null
          status: string
          agentType: string
        }>
      }
      ensureConnected?: (options?: {
        agentType?: string
        workingDir?: string
        conversationId?: number
        contextKey?: string
      }) => Promise<{
        contextKey: string
        connectionId: string
        status: string
      }>
    }
  }
}

const DEFAULT_RUN_ID = () =>
  `run-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`

function defaultClock(): PerfClock {
  return {
    now: () =>
      typeof performance !== "undefined" &&
      typeof performance.now === "function"
        ? performance.now()
        : Date.now(),
  }
}

function defaultRaf(): PerfRaf {
  return (cb) => {
    if (typeof requestAnimationFrame === "function") {
      return requestAnimationFrame(cb)
    }
    return window.setTimeout(
      () => cb(defaultClock().now()),
      16
    ) as unknown as number
  }
}

function defaultCancelRaf(): PerfCancelRaf {
  return (handle) => {
    if (typeof cancelAnimationFrame === "function") {
      cancelAnimationFrame(handle)
      return
    }
    clearTimeout(handle)
  }
}

function defaultSetTimer(): PerfSetTimer {
  return (cb, delay) => window.setTimeout(cb, delay) as unknown as number
}

function defaultClearTimer(): PerfClearTimer {
  return (handle) => {
    window.clearTimeout(handle)
  }
}

function readSupportedEntryTypes(
  override?: readonly string[]
): readonly string[] {
  if (override) return override
  try {
    const types = (
      PerformanceObserver as unknown as {
        supportedEntryTypes?: readonly string[]
      }
    ).supportedEntryTypes
    return types ?? []
  } catch {
    return []
  }
}

export class StreamingPerfRecorder {
  private active: ActiveRun | null = null
  private readonly clock: PerfClock
  private readonly raf: PerfRaf
  private readonly cancelRaf: PerfCancelRaf
  private readonly setTimer: PerfSetTimer
  private readonly clearTimer: PerfClearTimer
  private readonly supportedEntryTypes: readonly string[]
  private readonly PerformanceObserverImpl: typeof PerformanceObserver | null
  private readonly activeListeners = new Set<(active: boolean) => void>()
  private debugAllocations = 0
  /** Live publications deferred until after transaction complete (P0 order). */
  private queuedLivePublicationIds: number[] | null = null
  /** Delivery IDs for the envelope currently being applied (desktop path). */
  private currentDeliveryIds: readonly number[] | null = null

  constructor(options: StreamingPerfRecorderOptions = {}) {
    this.clock = options.clock ?? defaultClock()
    this.raf = options.raf ?? defaultRaf()
    this.cancelRaf = options.cancelRaf ?? defaultCancelRaf()
    this.setTimer = options.setTimer ?? defaultSetTimer()
    this.clearTimer = options.clearTimer ?? defaultClearTimer()
    this.supportedEntryTypes = readSupportedEntryTypes(
      options.supportedEntryTypes
    )
    this.PerformanceObserverImpl =
      options.performanceObserver === undefined
        ? typeof PerformanceObserver !== "undefined"
          ? PerformanceObserver
          : null
        : options.performanceObserver
  }

  isActive(): boolean {
    return this.active !== null
  }

  /** Test-only: allocations performed while a run is active. */
  debugAllocationCount(): number {
    return this.debugAllocations
  }

  subscribeActive(listener: (active: boolean) => void): () => void {
    this.activeListeners.add(listener)
    listener(this.active !== null)
    return () => {
      this.activeListeners.delete(listener)
    }
  }

  private notifyActive(active: boolean): void {
    for (const listener of this.activeListeners) {
      try {
        listener(active)
      } catch {
        // Listener failures must not break the recorder.
      }
    }
  }

  private touch(active: ActiveRun): void {
    active.lastActivityAt = this.clock.now()
  }

  private trackAllocation(active: ActiveRun): void {
    active.allocationCount += 1
    this.debugAllocations += 1
  }

  start(metadata: PerfRunMetadata = {}): void {
    this.stop()
    const now = this.clock.now()
    const active: ActiveRun = {
      metadata: {
        runId: metadata.runId ?? DEFAULT_RUN_ID(),
        seed: metadata.seed ?? 0,
        rateProfile: metadata.rateProfile ?? "eps_100",
        expectedEvents: metadata.expectedEvents ?? GROK_RICH_V1_EXPECTED_EVENTS,
        expectedTextSha256:
          metadata.expectedTextSha256 ?? GROK_RICH_V1_EXPECTED_TEXT_SHA256,
      },
      startedAt: now,
      lastActivityAt: now,
      deliveries: new Map(),
      pendingPaintIds: new Set(),
      inputProbes: new Map(),
      nextInputProbeId: 1,
      pipelineCounts: emptyPipelineCounts(),
      renderCounts: emptyRenderCounts(),
      receiptToTransactionMs: [],
      transactionToLivePublicationMs: [],
      batchToCommitMs: [],
      batchToPaintMs: [],
      inputToPaintMs: [],
      longTasksMs: [],
      frameGapsMs: [],
      eventLoopDriftMs: [],
      allocationCount: 0,
      longTaskObserver: null,
      rafHandle: null,
      driftTimerHandle: null,
      lastRafTimestamp: null,
      lastDriftScheduledAt: null,
      cadenceQueuedStartedAt: null,
      cadenceQueuedDurationMs: 0,
      cadencePaintCount: 0,
      cadenceFrozen: false,
      paintFlushIds: [],
      paintRafHandle: null,
      integrity: {
        expectedEvents: metadata.expectedEvents ?? GROK_RICH_V1_EXPECTED_EVENTS,
        appliedEvents: 0,
        firstSeq: 0,
        lastSeq: 0,
        duplicateCount: 0,
        gapCount: 0,
        finalTextSha256:
          metadata.expectedTextSha256 ?? GROK_RICH_V1_EXPECTED_TEXT_SHA256,
        ok: false,
      },
      deliverySnapshot: null,
      environment: null,
    }
    this.active = active
    this.debugAllocations = 0
    this.startObservers(active)
    this.notifyActive(true)
  }

  stop(): void {
    const active = this.active
    if (!active) return
    this.teardownObservers(active)
    this.active = null
    this.notifyActive(false)
  }

  private startObservers(active: ActiveRun): void {
    const supportsLongTask = this.supportedEntryTypes.includes("longtask")
    if (supportsLongTask && this.PerformanceObserverImpl) {
      try {
        const observer = new this.PerformanceObserverImpl((list) => {
          if (this.active !== active) return
          for (const entry of list.getEntries()) {
            active.longTasksMs.push(entry.duration)
            this.touch(active)
          }
        })
        observer.observe({ entryTypes: ["longtask"] })
        active.longTaskObserver = observer
        return
      } catch {
        // Fall through to RAF/timer fallback.
      }
    }

    // RAF-gap loop. Seed last timestamp at 0 so the first controlled frame
    // records an absolute gap (deterministic tests pass timestamps from 0).
    // Do not touch() here — continuous fallback sampling must not block quiet.
    active.lastRafTimestamp = 0
    const onFrame = (timestamp: number) => {
      if (this.active !== active) return
      if (active.lastRafTimestamp != null) {
        active.frameGapsMs.push(timestamp - active.lastRafTimestamp)
      }
      active.lastRafTimestamp = timestamp
      active.rafHandle = this.raf(onFrame)
    }
    active.rafHandle = this.raf(onFrame)

    // 50 ms event-loop drift loop (also does not touch lastActivityAt)
    const scheduleDrift = () => {
      if (this.active !== active) return
      active.lastDriftScheduledAt = this.clock.now()
      active.driftTimerHandle = this.setTimer(() => {
        if (this.active !== active) return
        const scheduledAt = active.lastDriftScheduledAt
        if (scheduledAt != null) {
          const elapsed = this.clock.now() - scheduledAt
          active.eventLoopDriftMs.push(elapsed - 50)
        }
        scheduleDrift()
      }, 50)
    }
    scheduleDrift()
  }

  private teardownObservers(active: ActiveRun): void {
    if (active.longTaskObserver) {
      try {
        active.longTaskObserver.disconnect()
      } catch {
        // ignore
      }
      active.longTaskObserver = null
    }
    if (active.rafHandle != null) {
      this.cancelRaf(active.rafHandle)
      active.rafHandle = null
    }
    if (active.paintRafHandle != null) {
      this.cancelRaf(active.paintRafHandle)
      active.paintRafHandle = null
    }
    if (active.driftTimerHandle != null) {
      this.clearTimer(active.driftTimerHandle)
      active.driftTimerHandle = null
    }
  }

  /**
   * Coalesce committed delivery IDs into a single next-paint RAF that is not
   * owned by React effect cleanup (so rapid re-renders cannot drop paints).
   */
  private enqueuePaintFlush(active: ActiveRun, ids: readonly number[]): void {
    for (const id of ids) {
      if (!active.paintFlushIds.includes(id)) {
        active.paintFlushIds.push(id)
      }
    }
    if (active.paintRafHandle != null) return
    active.paintRafHandle = this.raf(() => {
      if (this.active !== active) return
      active.paintRafHandle = null
      const toPaint = active.paintFlushIds
      active.paintFlushIds = []
      if (toPaint.length > 0) {
        this.markNextPaint(toPaint)
      }
    })
  }

  /** Freeze visual-UPS window so waitForQuiet / post-stream drain cannot inflate it. */
  private freezeCadence(active: ActiveRun): void {
    if (active.cadenceFrozen) return
    if (active.cadenceQueuedStartedAt != null) {
      // End at last pipeline activity, not wall-clock now (excludes quiet wait).
      active.cadenceQueuedDurationMs = Math.max(
        active.cadenceQueuedDurationMs,
        Math.max(0, active.lastActivityAt - active.cadenceQueuedStartedAt)
      )
    }
    active.cadenceFrozen = true
  }

  markBatchReceived(deliveryId: number, eventCount: number): void {
    const active = this.active
    if (!active) return
    this.trackAllocation(active)
    active.deliveries.set(deliveryId, {
      receivedAt: this.clock.now(),
      eventCount,
    })
    active.pipelineCounts.deliveryCallbacks += 1
    this.touch(active)
  }

  markTransactionComplete(deliveryIds: readonly number[]): void {
    const active = this.active
    if (!active) return
    const now = this.clock.now()
    for (const id of deliveryIds) {
      const delivery = active.deliveries.get(id)
      if (!delivery || delivery.transactionAt != null) continue
      delivery.transactionAt = now
      active.receiptToTransactionMs.push(now - delivery.receivedAt)
    }
    active.pipelineCounts.connectionTransactions += 1
    this.touch(active)
  }

  /**
   * One browser-frame commit: timestamp each unique delivery once, count one
   * ingestor frame, optionally one outer-map publication, and N connection
   * transactions for the changed connections in the frame.
   */
  markConnectionFrameCommitted(
    deliveryIds: readonly number[],
    changedConnections: number,
    mapPublished: boolean
  ): void {
    const active = this.active
    if (!active) return
    const now = this.clock.now()
    for (const id of deliveryIds) {
      const delivery = active.deliveries.get(id)
      if (!delivery || delivery.transactionAt != null) continue
      delivery.transactionAt = now
      active.receiptToTransactionMs.push(now - delivery.receivedAt)
    }
    active.pipelineCounts.ingestorFrames += 1
    if (mapPublished) {
      active.pipelineCounts.connectionMapPublications += 1
    }
    active.pipelineCounts.connectionTransactions += changedConnections
    this.touch(active)
  }

  /**
   * Bind delivery IDs for the in-flight desktop envelope so `setLiveMessage`
   * can attribute live publications without threading IDs through React props.
   */
  setCurrentDeliveryIds(deliveryIds: readonly number[] | null): void {
    this.currentDeliveryIds = deliveryIds
  }

  getCurrentDeliveryIds(): readonly number[] | null {
    return this.currentDeliveryIds
  }

  /**
   * Queue live-publication IDs so the desktop listener can mark transaction
   * complete first, then flush. Direct `markLivePublication` still works.
   */
  queueLivePublication(deliveryIds: readonly number[]): void {
    const active = this.active
    if (!active) return
    if (deliveryIds.length === 0) return
    this.queuedLivePublicationIds = [
      ...(this.queuedLivePublicationIds ?? []),
      ...deliveryIds,
    ]
  }

  flushQueuedLivePublication(): void {
    const ids = this.queuedLivePublicationIds
    this.queuedLivePublicationIds = null
    if (ids && ids.length > 0) {
      this.markLivePublication(ids)
    }
  }

  markLivePublication(deliveryIds: readonly number[]): void {
    const active = this.active
    if (!active) return
    const now = this.clock.now()
    for (const id of deliveryIds) {
      const delivery = active.deliveries.get(id)
      if (!delivery) continue
      if (delivery.liveAt == null) {
        delivery.liveAt = now
        if (delivery.transactionAt != null) {
          active.transactionToLivePublicationMs.push(
            Math.max(0, now - delivery.transactionAt)
          )
        }
      }
      active.pendingPaintIds.add(id)
    }
    active.pipelineCounts.livePublications += 1
    this.touch(active)
  }

  markReactCommit(): readonly number[] {
    const active = this.active
    if (!active) return []
    const now = this.clock.now()
    const ids = Array.from(active.pendingPaintIds)
    active.pendingPaintIds.clear()
    for (const id of ids) {
      const delivery = active.deliveries.get(id)
      if (!delivery) continue
      delivery.commitAt = now
      active.batchToCommitMs.push(now - delivery.receivedAt)
    }
    if (ids.length > 0) {
      active.pipelineCounts.reactCommits += 1
      this.touch(active)
      // Schedule paint outside React effect lifecycle so re-render cleanup
      // cannot cancel the RAF before the browser paints.
      this.enqueuePaintFlush(active, ids)
    }
    return ids
  }

  markNextPaint(deliveryIds: readonly number[]): void {
    const active = this.active
    if (!active) return
    if (deliveryIds.length === 0) return
    const now = this.clock.now()
    let painted = 0
    for (const id of deliveryIds) {
      const delivery = active.deliveries.get(id)
      if (!delivery || delivery.paintAt != null) continue
      delivery.paintAt = now
      active.batchToPaintMs.push(now - delivery.receivedAt)
      painted += 1
    }
    if (painted === 0) return
    active.pipelineCounts.paints += 1
    if (active.cadenceQueuedStartedAt != null && !active.cadenceFrozen) {
      active.cadencePaintCount += 1
      active.cadenceQueuedDurationMs = Math.max(
        active.cadenceQueuedDurationMs,
        Math.max(0, now - active.cadenceQueuedStartedAt)
      )
    }
    this.touch(active)
  }

  markInputQueued(): number {
    const active = this.active
    if (!active) return -1
    const id = active.nextInputProbeId++
    active.inputProbes.set(id, { queuedAt: this.clock.now() })
    if (active.cadenceQueuedStartedAt == null) {
      active.cadenceQueuedStartedAt = this.clock.now()
    }
    // Do not touch() — MessageInput probes every 100ms while active; refreshing
    // lastActivityAt would prevent waitForQuiet from ever completing.
    return id
  }

  markInputPaint(probeId: number): void {
    const active = this.active
    if (!active) return
    const probe = active.inputProbes.get(probeId)
    if (!probe || probe.paintAt != null) return
    const now = this.clock.now()
    probe.paintAt = now
    active.inputToPaintMs.push(now - probe.queuedAt)
    // Do not touch() — continuous input probes must not block quiet.
  }

  countRender(kind: PerfRenderKind): void {
    const active = this.active
    if (!active) return
    active.renderCounts[kind] += 1
  }

  setIntegrity(partial: Partial<StreamingPerfReport["integrity"]>): void {
    const active = this.active
    if (!active) return
    active.integrity = { ...active.integrity, ...partial }
  }

  setDeliverySnapshot(snapshot: EventBusMetricsSnapshot): void {
    const active = this.active
    if (!active) return
    active.deliverySnapshot = snapshot
  }

  setEnvironment(environment: StreamingPerfReport["environment"]): void {
    const active = this.active
    if (!active) return
    active.environment = environment
  }

  async waitForQuiet(quietMs = 250, timeoutMs = 5_000): Promise<void> {
    const active = this.active
    if (!active) return
    // Freeze UPS window at the start of quiet wait so drain time is excluded.
    this.freezeCadence(active)
    const startedAt = this.clock.now()
    await new Promise<void>((resolve, reject) => {
      let timer: number | null = null
      const finish = (error?: Error) => {
        if (timer !== null) this.clearTimer(timer)
        if (error) reject(error)
        else resolve()
      }
      const poll = () => {
        if (this.active !== active) {
          finish()
          return
        }
        const now = this.clock.now()
        if (now - startedAt >= timeoutMs) {
          finish(
            new Error(`streaming perf did not become quiet in ${timeoutMs}ms`)
          )
          return
        }
        if (now - active.lastActivityAt >= quietMs) {
          finish()
          return
        }
        timer = this.setTimer(poll, 25)
      }
      timer = this.setTimer(poll, 25)
    })
  }

  snapshot(): {
    batchToPaintMs: number[]
    inputToPaintMs: number[]
    longTasksMs: number[]
    frameGapsMs: number[]
    eventLoopDriftMs: number[]
    receiptToTransactionMs: number[]
    transactionToLivePublicationMs: number[]
    batchToCommitMs: number[]
    pipelineCounts: StreamingPerfReport["pipelineCounts"]
    renderCounts: StreamingPerfReport["renders"]
  } {
    const active = this.active
    if (!active) {
      return {
        batchToPaintMs: [],
        inputToPaintMs: [],
        longTasksMs: [],
        frameGapsMs: [],
        eventLoopDriftMs: [],
        receiptToTransactionMs: [],
        transactionToLivePublicationMs: [],
        batchToCommitMs: [],
        pipelineCounts: emptyPipelineCounts(),
        renderCounts: emptyRenderCounts(),
      }
    }
    return {
      batchToPaintMs: [...active.batchToPaintMs],
      inputToPaintMs: [...active.inputToPaintMs],
      longTasksMs: [...active.longTasksMs],
      frameGapsMs: [...active.frameGapsMs],
      eventLoopDriftMs: [...active.eventLoopDriftMs],
      receiptToTransactionMs: [...active.receiptToTransactionMs],
      transactionToLivePublicationMs: [
        ...active.transactionToLivePublicationMs,
      ],
      batchToCommitMs: [...active.batchToCommitMs],
      pipelineCounts: { ...active.pipelineCounts },
      renderCounts: { ...active.renderCounts },
    }
  }

  buildReport(
    overrides: {
      delivery?: EventBusMetricsSnapshot
      environment?: StreamingPerfReport["environment"]
      integrity?: Partial<StreamingPerfReport["integrity"]>
    } = {}
  ): StreamingPerfReport {
    const active = this.active
    if (!active) {
      throw new Error("streaming perf recorder is not active")
    }

    // Prefer frozen cadence (set at quiet / last paint); never extend with
    // post-stream quiet-drain wall time.
    if (!active.cadenceFrozen) {
      this.freezeCadence(active)
    }

    const integrity = {
      ...active.integrity,
      ...overrides.integrity,
    }
    // Always recompute integrity.ok from factual fields (do not sticky-or).
    integrity.ok =
      integrity.appliedEvents === integrity.expectedEvents &&
      integrity.finalTextSha256 === active.metadata.expectedTextSha256 &&
      integrity.gapCount === 0

    const updatesPerSecond =
      active.cadenceQueuedDurationMs > 0
        ? (active.cadencePaintCount / active.cadenceQueuedDurationMs) * 1000
        : 0

    const acceptance = evaluateStreamingPerf({
      batchToPaintMs: active.batchToPaintMs,
      inputToPaintMs: active.inputToPaintMs,
      longTasksMs: active.longTasksMs,
      frameGapsMs: active.frameGapsMs,
      eventLoopDriftMs: active.eventLoopDriftMs,
      visualUpdatesPerSecond: updatesPerSecond,
      integrityOk: integrity.ok,
    })

    const emptyDelivery: EventBusMetricsSnapshot = {
      emitted_count: 0,
      lagged_count: 0,
      ring_buffer_evict_count: 0,
      replay_count: 0,
      replay_event_total: 0,
      snapshot_fallback_count: 0,
      snapshot_cold_count: 0,
      forwarder_lagged_count: 0,
      worker_queue_full_count: 0,
      desktop_raw_envelope_count: 0,
      desktop_raw_bytes: 0,
      desktop_emit_attempt_count: 0,
      desktop_serialization_failure_count: 0,
      desktop_emit_failure_count: 0,
      desktop_legacy_emit_count: 0,
      desktop_batch_count: 0,
      desktop_batch_event_count: 0,
      desktop_batch_bytes: 0,
      desktop_batch_max_events: 0,
      desktop_batch_max_bytes: 0,
      desktop_batch_latency_total_us: 0,
      desktop_batch_latency_max_us: 0,
      desktop_queue_full_count: 0,
      desktop_startup_fallback_count: 0,
      desktop_runtime_failure_count: 0,
    }

    const environment = overrides.environment ??
      active.environment ?? {
        platform:
          typeof navigator !== "undefined" ? navigator.platform : "unknown",
        userAgent:
          typeof navigator !== "undefined" ? navigator.userAgent : "unknown",
        webviewVersion:
          typeof navigator !== "undefined"
            ? extractWebviewVersion(navigator.userAgent)
            : null,
        buildMode:
          process.env.NODE_ENV === "production" ? "production" : "development",
        hardwareAcceleration: "unknown",
        deliveryMode: "legacy",
        flags: legacyStreamingPerformanceFlags(),
      }

    return {
      schemaVersion: 1,
      runId: active.metadata.runId,
      fixture: {
        id: "grok_rich_v1",
        version: "grok-rich-v1",
        seed: active.metadata.seed,
        rateProfile: active.metadata.rateProfile,
        expectedEvents: active.metadata.expectedEvents,
        expectedTextChars: GROK_RICH_V1_EXPECTED_TEXT_CHARS,
        expectedTextSha256: active.metadata.expectedTextSha256,
      },
      environment,
      delivery: overrides.delivery ?? active.deliverySnapshot ?? emptyDelivery,
      pipelineCounts: { ...active.pipelineCounts },
      timings: {
        receiptToTransaction: summarizeSamples(active.receiptToTransactionMs),
        transactionToLivePublication: summarizeSamples(
          active.transactionToLivePublicationMs
        ),
        batchToCommit: summarizeSamples(active.batchToCommitMs),
        batchToPaint: summarizeSamples(active.batchToPaintMs),
        inputToPaint: summarizeSamples(active.inputToPaintMs),
        longTasks: summarizeSamples(active.longTasksMs),
        frameGaps: summarizeSamples(active.frameGapsMs),
        eventLoopDrift: summarizeSamples(active.eventLoopDriftMs),
      },
      renders: { ...active.renderCounts },
      integrity,
      cadence: {
        queuedDurationMs: active.cadenceQueuedDurationMs,
        paintCount: active.cadencePaintCount,
        updatesPerSecond,
      },
      resources: {
        markdownCacheEntries: null,
        markdownCacheBytes: null,
        highlightCacheEntries: null,
        highlightCacheBytes: null,
        liveConversations: null,
        liveSegments: null,
        liveTools: null,
        usedHeapBytes:
          typeof performance !== "undefined" &&
          "memory" in performance &&
          typeof (
            performance as Performance & {
              memory?: { usedJSHeapSize?: number }
            }
          ).memory?.usedJSHeapSize === "number"
            ? (
                performance as Performance & {
                  memory: { usedJSHeapSize: number }
                }
              ).memory.usedJSHeapSize
            : null,
        heapMeasurement:
          typeof performance !== "undefined" &&
          "memory" in performance &&
          typeof (
            performance as Performance & {
              memory?: { usedJSHeapSize?: number }
            }
          ).memory?.usedJSHeapSize === "number"
            ? "supported"
            : "unsupported",
      },
      acceptance,
    }
  }
}

export const streamingPerfRecorder = new StreamingPerfRecorder()

/**
 * Non-mutating input-latency probe: MessageChannel microtask → next paint.
 * Never touches the editor or synthesizes InputEvents.
 */
export function probeInputToPaint(recorder: StreamingPerfRecorder): void {
  if (!recorder.isActive()) return
  const probeId = recorder.markInputQueued()
  if (probeId < 0) return
  if (typeof MessageChannel === "undefined") {
    requestAnimationFrame(() => recorder.markInputPaint(probeId))
    return
  }
  const channel = new MessageChannel()
  channel.port1.onmessage = () => {
    requestAnimationFrame(() => recorder.markInputPaint(probeId))
    channel.port1.close()
    channel.port2.close()
  }
  channel.port2.postMessage(null)
}

export type { StreamingPerfReport, StreamingPerformanceFlags }
