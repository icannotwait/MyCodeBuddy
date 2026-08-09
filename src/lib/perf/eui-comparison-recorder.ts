export const LONG_FRAME_MS = 50

export type ComparisonShell = "eui" | "webview"
export type ComparisonAgent = "grok" | "codex"

export interface ComparisonMetadata {
  shell: ComparisonShell
  agent: ComparisonAgent
  promptId: string
  buildType: string
  backend: string
  gitCommit: string
  notes?: string
}

export interface ComparisonRun extends ComparisonMetadata {
  t0Ns: number
  tFirstTokenNs?: number
  tFirstPresentedNs?: number
  tEndNs?: number
  frameIntervalsMs: number[]
  longFrameThresholdMs: number
  longFrameCount: number
  peakShellRssKb?: number
  shellPid?: number
  rssScope: "shell-process-only"
  firstPresentedLatencyMs?: number
  frameIntervalP95Ms?: number
}

export type RafScheduler = (cb: (time: number) => void) => number

function nearestRankP95(intervals: number[]): number {
  if (intervals.length === 0) return 0
  const sorted = [...intervals].sort((a, b) => a - b)
  const index = Math.ceil(0.95 * sorted.length) - 1
  return sorted[Math.min(index, sorted.length - 1)]!
}

export function summarizeFrameIntervals(
  frames: number[],
  t0: number,
  firstPresented: number,
  end: number,
): Pick<
  ComparisonRun,
  | "frameIntervalsMs"
  | "frameIntervalP95Ms"
  | "longFrameCount"
  | "longFrameThresholdMs"
  | "firstPresentedLatencyMs"
> {
  const active: number[] = []
  for (let i = 0; i + 1 < frames.length; i++) {
    const a = frames[i]!
    const b = frames[i + 1]!
    if (a >= firstPresented && b <= end && a < end) {
      active.push(b - a)
    }
  }
  let longFrameCount = 0
  for (const interval of active) {
    if (interval > LONG_FRAME_MS) longFrameCount++
  }
  return {
    frameIntervalsMs: active,
    frameIntervalP95Ms: nearestRankP95(active),
    longFrameCount,
    longFrameThresholdMs: LONG_FRAME_MS,
    firstPresentedLatencyMs: firstPresented - t0,
  }
}

export function recorderFromFrames(
  frames: number[],
  markers: {
    t0: number
    firstToken?: number
    firstPresented: number
    end: number
  },
) {
  return {
    finish(): ComparisonRun & {
      firstPresentedLatencyMs: number
      frameIntervalP95Ms: number
    } {
      const summary = summarizeFrameIntervals(
        frames,
        markers.t0,
        markers.firstPresented,
        markers.end,
      )
      return {
        shell: "eui",
        agent: "codex",
        promptId: "continuous-text-v1",
        buildType: "test",
        backend: "test",
        gitCommit: "test",
        t0Ns: markers.t0,
        tFirstTokenNs: markers.firstToken,
        tFirstPresentedNs: markers.firstPresented,
        tEndNs: markers.end,
        rssScope: "shell-process-only",
        ...summary,
        firstPresentedLatencyMs: summary.firstPresentedLatencyMs!,
        frameIntervalP95Ms: summary.frameIntervalP95Ms!,
      }
    },
  }
}

export class ManualRafScheduler {
  private queue: Array<(t: number) => void> = []
  private paintEligible = false

  request = (cb: (t: number) => void): number => {
    this.queue.push(cb)
    return this.queue.length
  }

  markPaint() {
    this.paintEligible = true
  }

  flushOne(time: number) {
    const cb = this.queue.shift()
    if (cb) cb(time)
  }

  get paintMarked() {
    return this.paintEligible
  }
}

export class ComparisonRecorder {
  firstPresentedNs: number | undefined
  private t0Ns: number | undefined
  private tEndNs: number | undefined
  private tFirstTokenNs: number | undefined
  private armed = false
  private raf1: number | undefined
  private raf2: number | undefined
  private frameTimes: number[] = []
  private finished = false
  private readonly requestRaf: RafScheduler

  constructor(
    private readonly metadata: ComparisonMetadata,
    requestRaf: RafScheduler = (cb) =>
      typeof requestAnimationFrame !== "undefined"
        ? requestAnimationFrame(cb)
        : 0,
  ) {
    if (metadata.shell !== "webview" && metadata.shell !== "eui") {
      throw new Error("invalid shell")
    }
    if (metadata.agent !== "grok" && metadata.agent !== "codex") {
      throw new Error("invalid agent")
    }
    for (const key of ["promptId", "buildType", "backend", "gitCommit"] as const) {
      if (!metadata[key]) throw new Error(`missing ${key}`)
    }
    this.requestRaf = requestRaf
  }

  markT0(ns: number) {
    this.t0Ns = ns
  }

  markFirstToken(ns: number) {
    if (this.tFirstTokenNs === undefined) this.tFirstTokenNs = ns
  }

  assistantCommitted(_text: string) {
    if (this.finished || this.firstPresentedNs !== undefined) return
    this.armed = true
    // RAF1 then RAF2 after paint opportunity
    this.raf1 = this.requestRaf(() => {
      this.raf2 = this.requestRaf((t) => {
        if (this.firstPresentedNs === undefined) {
          this.firstPresentedNs = t
          this.frameTimes.push(t)
        }
      })
    })
  }

  markEnd(ns: number) {
    this.tEndNs = ns
  }

  sampleFrame(ns: number) {
    if (this.firstPresentedNs !== undefined && this.tEndNs === undefined) {
      this.frameTimes.push(ns)
    }
  }

  finish(): ComparisonRun {
    this.finished = true
    const t0 = this.t0Ns ?? 0
    const firstPresented = this.firstPresentedNs ?? t0
    const end = this.tEndNs ?? firstPresented
    const summary = summarizeFrameIntervals(
      this.frameTimes.length >= 2
        ? this.frameTimes
        : [firstPresented, end],
      t0,
      firstPresented,
      end,
    )
    return {
      ...this.metadata,
      t0Ns: t0,
      tFirstTokenNs: this.tFirstTokenNs,
      tFirstPresentedNs: this.firstPresentedNs,
      tEndNs: this.tEndNs,
      rssScope: "shell-process-only",
      ...summary,
    }
  }
}

declare global {
  interface Window {
    __codegEuiComparison?: {
      start: (meta: ComparisonMetadata) => void
      finish: () => ComparisonRun | undefined
    }
  }
}

let active: ComparisonRecorder | undefined

export function installComparisonApi(requestRaf?: RafScheduler) {
  if (typeof window === "undefined") return
  window.__codegEuiComparison = {
    start(meta) {
      active = new ComparisonRecorder(meta, requestRaf)
    },
    finish() {
      const run = active?.finish()
      active = undefined
      return run
    },
  }
}

export function getActiveComparisonRecorder() {
  return active
}
