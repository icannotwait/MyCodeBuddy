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
 * Primary: `transition_at` (sub-second wall time of the phase transition).
 * Secondary: host-global `transition_seq` for equal-millisecond ties.
 * Does not fall back to lease-local `grace_deadline` or per-lease `version` —
 * those are not connection-wide sequences and invert same-second order.
 * Legacy empty `transition_at` uses `last_progress_at` only; missing seq → 0.
 */
export function diagnosticOrderKey(
  p: ToolWatchdogProjection
): [string, number] {
  const at =
    p.transition_at && p.transition_at.length > 0
      ? p.transition_at
      : p.last_progress_at
  return [at, p.transition_seq ?? 0]
}

export function isNewerDiagnostic(
  candidate: ToolWatchdogProjection,
  current: ToolWatchdogProjection
): boolean {
  const [cAt, cSeq] = diagnosticOrderKey(candidate)
  const [bAt, bSeq] = diagnosticOrderKey(current)
  if (cAt !== bAt) return cAt > bAt
  return cSeq >= bSeq
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
