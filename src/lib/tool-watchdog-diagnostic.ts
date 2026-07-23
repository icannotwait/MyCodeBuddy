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
  /** ISO timestamp of the transition / last progress stamp. */
  timestamp: string
  cancellation_scope: CancellationScope | null
  error_code: string | null
}

/** Project a public wire projection down to session-details fields only. */
export function toSecretSafeDiagnostic(
  projection: ToolWatchdogProjection
): ToolWatchdogDiagnostic {
  return {
    phase: projection.phase,
    tool_title: projection.tool_title,
    timestamp: projection.last_progress_at,
    cancellation_scope: projection.cancellation_scope ?? null,
    error_code: projection.error_code ?? null,
  }
}

/**
 * Prefer the highest-version live projection; fall back to the last terminal
 * transition retained on the connection (timed_out / cleared).
 */
export function pickLatestToolWatchdogDiagnostic(
  live: Record<string, ToolWatchdogProjection> | undefined | null,
  last: ToolWatchdogProjection | null | undefined
): ToolWatchdogDiagnostic | null {
  let best: ToolWatchdogProjection | null = null
  if (live) {
    for (const p of Object.values(live)) {
      if (!best || p.version > best.version) best = p
    }
  }
  if (last) {
    if (!best || last.version >= best.version) best = last
  }
  return best ? toSecretSafeDiagnostic(best) : null
}

/** Assert a diagnostic object has no secret-looking fields (test helper). */
export function diagnosticFieldKeys(
  d: ToolWatchdogDiagnostic
): (keyof ToolWatchdogDiagnostic)[] {
  return Object.keys(d) as (keyof ToolWatchdogDiagnostic)[]
}
