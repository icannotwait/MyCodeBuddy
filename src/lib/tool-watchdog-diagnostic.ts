import type {
  CancellationScope,
  ToolWatchdogPhase,
  ToolWatchdogProjection,
  ToolWatchdogTitle,
} from "@/lib/types"

/**
 * Secret-safe session-details diagnostic for the most recent watchdog
 * transition. Never includes tool input, env, prompts, or provider tool ids.
 */
export interface ToolWatchdogDiagnostic {
  phase: ToolWatchdogPhase
  tool_title: ToolWatchdogTitle
  /** ISO timestamp of the phase transition (not last_progress_at). */
  timestamp: string
  cancellation_scope: CancellationScope | null
  error_code: string | null
}

/**
 * Ordering key for "latest diagnostic" across concurrent leases.
 * Prefers `transition_at` (wall time of the phase transition). Per-lease
 * versions are last-resort only — every new lease starts at version 1.
 */
export function diagnosticOrderKey(
  p: ToolWatchdogProjection
): [string, string, string, number] {
  const transition =
    p.transition_at && p.transition_at.length > 0
      ? p.transition_at
      : (p.grace_deadline ?? p.last_progress_at)
  return [
    transition,
    p.grace_deadline ?? "",
    p.last_progress_at,
    p.version,
  ]
}

export function isNewerDiagnostic(
  candidate: ToolWatchdogProjection,
  current: ToolWatchdogProjection
): boolean {
  const a = diagnosticOrderKey(candidate)
  const b = diagnosticOrderKey(current)
  for (let i = 0; i < a.length; i++) {
    if (a[i] > b[i]) return true
    if (a[i] < b[i]) return false
  }
  return true
}

/** Project a public wire projection down to session-details fields only. */
export function toSecretSafeDiagnostic(
  projection: ToolWatchdogProjection
): ToolWatchdogDiagnostic {
  const timestamp =
    projection.transition_at && projection.transition_at.length > 0
      ? projection.transition_at
      : (projection.grace_deadline ?? projection.last_progress_at)
  return {
    phase: projection.phase,
    tool_title: projection.tool_title,
    timestamp,
    cancellation_scope: projection.cancellation_scope ?? null,
    error_code: projection.error_code ?? null,
  }
}

/**
 * Prefer the latest transition by wall time / event order among live
 * projections and the last terminal transition retained on the connection.
 */
export function pickLatestToolWatchdogDiagnostic(
  live: Record<string, ToolWatchdogProjection> | undefined | null,
  last: ToolWatchdogProjection | null | undefined
): ToolWatchdogDiagnostic | null {
  let best: ToolWatchdogProjection | null = null
  if (live) {
    for (const p of Object.values(live)) {
      if (!best || isNewerDiagnostic(p, best)) best = p
    }
  }
  if (last) {
    if (!best || isNewerDiagnostic(last, best)) best = last
  }
  return best ? toSecretSafeDiagnostic(best) : null
}

/** Assert a diagnostic object has no secret-looking fields (test helper). */
export function diagnosticFieldKeys(
  d: ToolWatchdogDiagnostic
): (keyof ToolWatchdogDiagnostic)[] {
  return Object.keys(d) as (keyof ToolWatchdogDiagnostic)[]
}
