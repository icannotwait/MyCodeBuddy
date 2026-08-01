import type { DelegationRuntimeStats, DelegationTouchedFile } from "@/lib/types"

export const STICKY_ORPHAN_TIMEOUT_MS = 900_000

export type WorkUnitRunObservation = {
  identity: string
  taskId: string | null
  lifecycleStatus: "running" | "ok" | "err"
  errorCode: string | null
  startedAt: string | null
  finishedAt: string | null
  lastAgentActivityAt: string | null
  runtimeStats: DelegationRuntimeStats | null
  current: boolean
}

export type WorkUnitRuntimeProjection = {
  activeSticky: boolean
  startedAt: string | null
  finishedAt: string | null
  elapsedMs: number | null
  runtimeStats: DelegationRuntimeStats | null
  toolCallCount: number | null
  lifecycleOverride: "running" | null
  statusOverride: "running" | null
  suppressErrorCode: boolean
}

const RECOVERABLE_ERROR_CODES: ReadonlySet<string> = new Set([
  "parent_turn_failed",
  "join_abandoned",
  "parent_disconnected",
])

type TimedValue = {
  value: string
  ms: number
}

type RunPeaks = {
  hasStats: boolean
  toolCallCount: number | null
  editToolCallCount: number | null
  additions: number | null
  deletions: number | null
  lineCountsComplete: boolean
}

/** Per-run wall anchors used to sum active durations (excludes inter-run idle). */
type RunTiming = {
  started: TimedValue | null
  finished: TimedValue | null
  /** True when this run is the current observation and still open for sticky tick. */
  isCurrentOpen: boolean
}

function finiteNonNegative(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) && value >= 0
    ? value
    : null
}

function timedValue(value: string | null | undefined): TimedValue | null {
  if (!value) return null
  const ms = Date.parse(value)
  return Number.isFinite(ms) ? { value, ms } : null
}

function earlier(
  current: TimedValue | null,
  candidate: string | null | undefined
): TimedValue | null {
  const parsed = timedValue(candidate)
  if (!parsed || (current && current.ms <= parsed.ms)) return current
  return parsed
}

function later(
  current: TimedValue | null,
  candidate: string | null | undefined
): TimedValue | null {
  const parsed = timedValue(candidate)
  if (!parsed || (current && current.ms > parsed.ms)) return current
  return parsed
}

function peak(current: number | null, candidate: unknown): number | null {
  const parsed = finiteNonNegative(candidate)
  if (parsed == null) return current
  return current == null ? parsed : Math.max(current, parsed)
}

function isRecoverable(
  observation: WorkUnitRunObservation,
  explicitUserCancel: boolean
): boolean {
  if (observation.lifecycleStatus !== "err" || !observation.errorCode) {
    return false
  }
  if (observation.errorCode === "parent_canceled") {
    return !explicitUserCancel
  }
  return RECOVERABLE_ERROR_CODES.has(observation.errorCode)
}

function foldRunTiming(
  current: RunTiming | undefined,
  observation: WorkUnitRunObservation
): RunTiming {
  const stats = observation.runtimeStats
  let started = current?.started ?? null
  let finished = current?.finished ?? null
  started = earlier(started, observation.startedAt)
  finished = later(finished, observation.finishedAt)
  if (stats) {
    started = earlier(started, stats.started_at)
    finished = later(finished, stats.finished_at)
  }
  // Current observation wins for "open" — last writer in the scan may not be
  // current, so only set true when this observation is current+running.
  const isCurrentOpen =
    observation.current && observation.lifecycleStatus === "running"
      ? true
      : (current?.isCurrentOpen ?? false)
  return { started, finished, isCurrentOpen }
}

/**
 * Sum each run's active span (finished−started, or now−started for the open
 * current run). Inter-run idle gaps are excluded so multi-turn / continue
 * work units report accumulated turn time, matching per-run tool peaks.
 */
function sumPerRunElapsedMs(
  timings: Iterable<RunTiming>,
  nowMs: number
): number | null {
  let total = 0
  let any = false
  for (const timing of timings) {
    if (!timing.started) continue
    // Open current run ticks against now; completed runs use finish.
    const resolvedEnd =
      timing.isCurrentOpen && Number.isFinite(nowMs)
        ? nowMs
        : timing.finished?.ms
    if (resolvedEnd == null) continue
    const elapsed = resolvedEnd - timing.started.ms
    if (elapsed < 0) continue
    total += elapsed
    any = true
  }
  return any ? total : null
}

export function buildDelegationWorkUnitRuntime(input: {
  runs: readonly WorkUnitRunObservation[]
  nowMs: number
  hasLiveBinding: boolean
  explicitUserCancel: boolean
}): WorkUnitRuntimeProjection {
  const peaksByRun = new Map<string, RunPeaks>()
  const timingByRun = new Map<string, RunTiming>()
  const touchedFiles = new Map<string, DelegationTouchedFile>()
  let touchedFilesTruncated = false
  let startedAt: TimedValue | null = null
  let latestFinishedAt: TimedValue | null = null
  let orphanReference: TimedValue | null = null
  let currentObservation: WorkUnitRunObservation | null = null

  for (const observation of input.runs) {
    if (observation.current) currentObservation = observation
    startedAt = earlier(startedAt, observation.startedAt)
    latestFinishedAt = later(latestFinishedAt, observation.finishedAt)
    orphanReference = later(orphanReference, observation.finishedAt)
    orphanReference = later(orphanReference, observation.lastAgentActivityAt)
    orphanReference = later(orphanReference, observation.startedAt)

    const runKey = observation.taskId ?? observation.identity
    timingByRun.set(runKey, foldRunTiming(timingByRun.get(runKey), observation))

    const stats = observation.runtimeStats
    if (!stats) continue
    startedAt = earlier(startedAt, stats.started_at)
    latestFinishedAt = later(latestFinishedAt, stats.finished_at)
    orphanReference = later(orphanReference, stats.finished_at)
    orphanReference = later(orphanReference, stats.started_at)
    touchedFilesTruncated ||= stats.touched_files_truncated
    for (const file of stats.touched_files) {
      touchedFiles.set(file.path, { ...file })
    }

    const runPeaks = peaksByRun.get(runKey) ?? {
      hasStats: false,
      toolCallCount: null,
      editToolCallCount: null,
      additions: null,
      deletions: null,
      lineCountsComplete: false,
    }
    runPeaks.hasStats = true
    runPeaks.toolCallCount = peak(runPeaks.toolCallCount, stats.tool_call_count)
    runPeaks.editToolCallCount = peak(
      runPeaks.editToolCallCount,
      stats.edit_tool_call_count
    )
    runPeaks.additions = peak(runPeaks.additions, stats.additions)
    runPeaks.deletions = peak(runPeaks.deletions, stats.deletions)
    runPeaks.lineCountsComplete = stats.line_counts_complete
    peaksByRun.set(runKey, runPeaks)
  }

  currentObservation ??= input.runs[input.runs.length - 1] ?? null
  const recoverable = currentObservation
    ? isRecoverable(currentObservation, input.explicitUserCancel)
    : false
  let activeSticky = false
  if (!input.explicitUserCancel && currentObservation) {
    if (currentObservation.lifecycleStatus === "running") {
      activeSticky = true
    } else if (recoverable) {
      if (input.hasLiveBinding) {
        activeSticky = true
      } else if (orphanReference && Number.isFinite(input.nowMs)) {
        activeSticky =
          input.nowMs - orphanReference.ms < STICKY_ORPHAN_TIMEOUT_MS
      }
    }
  }

  const finishedAt = activeSticky ? null : (latestFinishedAt?.value ?? null)
  // Prefer sum of per-run active spans over unit wall-clock so idle gaps
  // between continue/replace turns are not counted as work time.
  const elapsedMs = sumPerRunElapsedMs(timingByRun.values(), input.nowMs)

  const runPeaks = [...peaksByRun.values()].filter((entry) => entry.hasStats)
  const toolPeaks = runPeaks
    .map((entry) => entry.toolCallCount)
    .filter((value): value is number => value != null)
  const editPeaks = runPeaks
    .map((entry) => entry.editToolCallCount)
    .filter((value): value is number => value != null)
  const additionPeaks = runPeaks
    .map((entry) => entry.additions)
    .filter((value): value is number => value != null)
  const deletionPeaks = runPeaks
    .map((entry) => entry.deletions)
    .filter((value): value is number => value != null)
  const toolCallCount =
    toolPeaks.length > 0
      ? toolPeaks.reduce((total, value) => total + value, 0)
      : null
  const runtimeStartedAt = startedAt?.value ?? null

  let runtimeStats: DelegationRuntimeStats | null = null
  if (
    runPeaks.length > 0 &&
    toolCallCount != null &&
    runtimeStartedAt != null
  ) {
    const editToolCallCount = editPeaks.reduce(
      (total, value) => total + value,
      0
    )
    const additions =
      additionPeaks.length > 0
        ? additionPeaks.reduce((total, value) => total + value, 0)
        : null
    const deletions =
      deletionPeaks.length > 0
        ? deletionPeaks.reduce((total, value) => total + value, 0)
        : null
    runtimeStats = {
      started_at: runtimeStartedAt,
      finished_at: finishedAt,
      tool_call_count: toolCallCount,
      edit_tool_call_count: editToolCallCount,
      touched_files: [...touchedFiles.values()],
      touched_files_truncated: touchedFilesTruncated,
      additions,
      deletions,
      line_counts_complete:
        runPeaks.every((entry) => entry.lineCountsComplete) &&
        additionPeaks.length === runPeaks.length &&
        deletionPeaks.length === runPeaks.length,
    }
  }

  return {
    activeSticky,
    startedAt: startedAt?.value ?? null,
    finishedAt,
    elapsedMs,
    runtimeStats,
    toolCallCount,
    lifecycleOverride: activeSticky ? "running" : null,
    statusOverride: activeSticky ? "running" : null,
    suppressErrorCode:
      activeSticky &&
      recoverable &&
      currentObservation?.lifecycleStatus === "err",
  }
}
