/**
 * Turns a blocked/failed inbox card's machine `failure_sig` / `reason` (written
 * by the engine, e.g. `empty_diff:implement`, `validation_failed:<hash>`,
 * `no_artifacts:design`, or a `reason` like `max_attempts`) into a human cause
 * (D9). Pure — returns an i18n key under `Loops.failureSig` plus a `family` for
 * grouping/labeling, or null when the card's own generic description is the best
 * we have. The vocabulary mirrors the engine's call sites in
 * `src-tauri/src/loop_engine/{gates,dispatch,driver}.rs`.
 */

import type { LoopInboxItemRow } from "./types"

/** Message keys under `Loops.failureSig` — kept a literal union so the typed
 *  next-intl `t(key)` call accepts the result without a cast. */
export type FailureSigKey =
  | "emptyDiff"
  | "validationFailed"
  | "noArtifacts"
  | "infraFailure"
  | "stalled"
  | "maxAttempts"
  | "repeatedFailure"
  | "validationUnrunnable"
  | "dependencyUnsatisfiable"
  | "worktreeUnavailable"
  | "worktreeDirty"
  | "noIntegrationCriteria"
  | "integrationGap"
  | "designRejected"
  | "oscillation"

export interface HumanFailure {
  /** Cause family — a short grouping label (used for tests / future grouping). */
  family: string
  /** The human sentence — a message key (`Loops.failureSig.*`). */
  key: FailureSigKey
}

function payloadObj(p: unknown): Record<string, unknown> {
  return p && typeof p === "object" ? (p as Record<string, unknown>) : {}
}

function str(v: unknown): string | null {
  return typeof v === "string" && v.length > 0 ? v : null
}

/** The integer attempt a card recorded (engine writes `attempt` on blocked
 *  cards), or null when absent / not positive. Shown as card meta, not folded
 *  into the humanized sentence. */
export function failureAttempt(item: LoopInboxItemRow): number | null {
  const a = payloadObj(item.payload).attempt
  return typeof a === "number" && Number.isInteger(a) && a > 0 ? a : null
}

export function humanizeFailureSig(
  item: LoopInboxItemRow
): HumanFailure | null {
  const p = payloadObj(item.payload)
  const sig = str(p.failure_sig)
  const reason = str(p.reason)

  // D14: an oscillation card is a deterministic-failure escalation. Its message
  // takes precedence over the underlying `failure_sig` it carries (the repeated
  // cause), because the actionable point is "retry won't help — use an exit".
  if (reason === "oscillation") {
    return { family: "oscillation", key: "oscillation" }
  }

  // The failure_sig (what actually failed) is more informative than the breaker
  // reason (why we stopped, e.g. max_attempts), so it wins when both are present.
  if (sig) {
    const prefix = sig.split(":")[0]
    switch (prefix) {
      case "empty_diff":
        return { family: "emptyDiff", key: "emptyDiff" }
      case "validation_failed":
        return { family: "validation", key: "validationFailed" }
      case "no_artifacts":
        return { family: "noArtifacts", key: "noArtifacts" }
      case "infra_failure":
        return { family: "infra", key: "infraFailure" }
    }
  }

  switch (reason) {
    case "stalled":
      return { family: "stalled", key: "stalled" }
    case "max_attempts":
      return { family: "breaker", key: "maxAttempts" }
    case "repeated_failure":
      return { family: "breaker", key: "repeatedFailure" }
    case "validation_unrunnable":
      return { family: "validation", key: "validationUnrunnable" }
    case "dependency_unsatisfiable":
      return { family: "dependency", key: "dependencyUnsatisfiable" }
    case "worktree_unavailable":
      return { family: "infra", key: "worktreeUnavailable" }
    case "worktree_dirty_before_finalize":
      return { family: "dirty", key: "worktreeDirty" }
    case "no_integration_criteria":
      return { family: "integration", key: "noIntegrationCriteria" }
    case "integration_gap_exhausted":
      return { family: "integration", key: "integrationGap" }
    case "design_rejected_exhausted":
      return { family: "designRejected", key: "designRejected" }
  }
  return null
}
