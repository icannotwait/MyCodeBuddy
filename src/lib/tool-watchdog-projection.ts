import type { ToolWatchdogProjection } from "@/lib/types"

/**
 * Pure multi-window reduction: apply an authoritative projection into a
 * lease_id → projection map. Never invents local terminal state.
 * - Newer version wins; older version is ignored.
 * - `cleared` / `timed_out` remove the key when version is not older.
 */
export function reduceToolWatchdogProjection(
  map: Record<string, ToolWatchdogProjection>,
  projection: ToolWatchdogProjection
): Record<string, ToolWatchdogProjection> {
  const existing = map[projection.lease_id]
  if (existing && existing.version > projection.version) {
    return map
  }

  if (projection.phase === "cleared" || projection.phase === "timed_out") {
    if (!existing) return map
    if (existing.version > projection.version) return map
    if (!(projection.lease_id in map)) return map
    const next = { ...map }
    delete next[projection.lease_id]
    return next
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
    return map
  }

  return { ...map, [projection.lease_id]: projection }
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
