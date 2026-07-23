import type { ToolWatchdogProjection } from "@/lib/types"

/**
 * Pure multi-window reduction: apply an authoritative projection into a
 * lease_id → projection map. Never invents local terminal state.
 * - Newer version wins; older version is ignored.
 * - `cleared` / `timed_out` remove the key when version is not older.
 * - After terminal remove, a max-version tombstone rejects later lower (or
 *   equal) actionable projections for the same lease_id so a late Cancelling
 *   emission cannot resurrect a banner.
 */
export function reduceToolWatchdogProjection(
  map: Record<string, ToolWatchdogProjection>,
  projection: ToolWatchdogProjection,
  maxVersionByLease: Record<string, number> = {}
): {
  map: Record<string, ToolWatchdogProjection>
  maxVersionByLease: Record<string, number>
} {
  const floor = maxVersionByLease[projection.lease_id] ?? 0
  const existing = map[projection.lease_id]
  const isTerminal =
    projection.phase === "cleared" || projection.phase === "timed_out"

  if (projection.version < floor) {
    return { map, maxVersionByLease }
  }
  if (existing && existing.version > projection.version) {
    return { map, maxVersionByLease }
  }
  // Tombstone after terminal remove: block equal-version actionable resurrect.
  if (!isTerminal && projection.version === floor && floor > 0 && !existing) {
    return { map, maxVersionByLease }
  }

  const nextMax = {
    ...maxVersionByLease,
    [projection.lease_id]: Math.max(floor, projection.version),
  }

  if (isTerminal) {
    if (existing && existing.version > projection.version) {
      return { map, maxVersionByLease }
    }
    // Always record the terminal version tombstone, even when the lease was
    // never in the local map (out-of-order TimedOut before Grace).
    if (!existing) {
      return { map, maxVersionByLease: nextMax }
    }
    const next = { ...map }
    delete next[projection.lease_id]
    return { map: next, maxVersionByLease: nextMax }
  }

  if (
    existing &&
    existing.version === projection.version &&
    existing.phase === projection.phase &&
    existing.grace_deadline === projection.grace_deadline &&
    existing.last_progress_at === projection.last_progress_at &&
    existing.transition_at === projection.transition_at &&
    existing.tool_title === projection.tool_title &&
    existing.error_code === projection.error_code &&
    existing.cancellation_scope === projection.cancellation_scope
  ) {
    return { map, maxVersionByLease: nextMax }
  }

  return {
    map: { ...map, [projection.lease_id]: projection },
    maxVersionByLease: nextMax,
  }
}

/** Remaining grace as whole seconds, clamped at 0 (countdown boundary). */
export function remainingGraceSeconds(
  graceDeadline: string | null | undefined,
  nowMs: number
): number | null {
  if (!graceDeadline) return null
  const t = Date.parse(graceDeadline)
  if (Number.isNaN(t)) return null
  return Math.max(0, Math.ceil((t - nowMs) / 1000))
}

export function formatCountdown(totalSeconds: number): string {
  const s = Math.max(0, Math.floor(totalSeconds))
  const m = Math.floor(s / 60)
  const r = s % 60
  return `${m}:${r.toString().padStart(2, "0")}`
}
