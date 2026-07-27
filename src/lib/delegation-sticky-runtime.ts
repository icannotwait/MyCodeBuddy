/**
 * Pure sticky work-unit runtime helpers for parent delegation cards.
 *
 * Display-only: identity namespacing, peak-by-task tool fold, recovery-gated
 * phase machine, orphan clock evaluation, and latest-only card eligibility.
 * Store / React wiring lives elsewhere.
 */

export type StickyUnit =
  | { kind: "work_unit"; workUnitKey: string }
  | { kind: "parent_child"; childConversationId: number }
  | { kind: "task"; taskId: string }

export type StickyIdentity = {
  backendCacheKey: string
  parentConversationId: number
  unit: StickyUnit
}

export type StickyKeyResult =
  | StickyIdentity
  | { kind: "task_only"; backendCacheKey: string; taskId: string }

export type RecoverySignals = {
  liveBindingRunning: boolean
  childProjectionRunning: boolean
  activeRunNonTerminal: boolean
  openAttention: boolean
  /** True only when wait set intersects this unit’s child/task — not global wait alone */
  parentWaitingForThisChild: boolean
  continueOrReplaceAdmitted: boolean
}

export type StickyObservation = {
  type:
    | "running"
    | "stats"
    | "canceled"
    | "completed"
    | "failed"
    | "tick"
    | "reseed"
  taskId: string
  nowMs: number
  generation?: number | null
  parentToolUseId?: string | null
  startedAt?: string | null
  finishedAt?: string | null
  toolCallCount?: number | null
  errorCode?: string | null
  cancelReason?: string | null
  recovery: RecoverySignals
}

export type TaskMeta = {
  generation: number | null
  startedAtMs: number | null
  finishedAtMs: number | null
  parentToolUseId: string | null
  /** Monotonic admission order for this unit (running-only). */
  admissionOrder: number
}

export type StickyPhase = "active_sticky" | "terminal"

export type StickyBucket = {
  identityKey: string
  phase: StickyPhase
  anchorStartedAtMs: number | null
  terminalElapsedMs: number | null
  peakByTaskId: Map<string, number>
  taskMeta: Map<string, TaskMeta>
  activeTaskId: string | null
  activeParentToolUseId: string | null
  activeGeneration: number | null
  orphanStartedAtMs: number | null
  lastDisplayToolCount: number
  /** Next admission order integer for newly admitted running tasks. */
  nextAdmissionOrder: number
}

/** 15 minutes — above the 600s continuation checkpoint. */
export const STICKY_ORPHAN_TIMEOUT_MS = 900_000

const ALWAYS_TERMINAL_ERROR_CODES = new Set([
  "parent_canceled",
  "cancel_delegation",
  "canceled",
  "parent_ended",
])

const RECOVERY_GATED_ERROR_CODES = new Set([
  "parent_turn_failed",
  "join_abandoned",
  "parent_disconnected",
])

const ALWAYS_TERMINAL_CANCEL_REASONS = new Set([
  "usercancel",
  "user_cancel",
  "cancel_delegation",
])

function isFiniteNonNegativeInteger(n: number): boolean {
  return Number.isFinite(n) && n >= 0 && Math.floor(n) === n
}

function parseTimestampMs(value: string | null | undefined): number | null {
  if (value == null || value === "") return null
  const ms = Date.parse(value)
  if (!Number.isFinite(ms)) return null
  return ms
}

function sumPeaks(peakByTaskId: ReadonlyMap<string, number>): number {
  let total = 0
  for (const v of peakByTaskId.values()) {
    total += v
  }
  return total
}

function clonePeaks(
  peakByTaskId: ReadonlyMap<string, number>
): Map<string, number> {
  return new Map(peakByTaskId)
}

function cloneTaskMeta(
  taskMeta: ReadonlyMap<string, TaskMeta>
): Map<string, TaskMeta> {
  return new Map(taskMeta)
}

function emptyBucket(identityKey: string): StickyBucket {
  return {
    identityKey,
    phase: "active_sticky",
    anchorStartedAtMs: null,
    terminalElapsedMs: null,
    peakByTaskId: new Map(),
    taskMeta: new Map(),
    activeTaskId: null,
    activeParentToolUseId: null,
    activeGeneration: null,
    orphanStartedAtMs: null,
    lastDisplayToolCount: 0,
    nextAdmissionOrder: 0,
  }
}

function isAdmitted(bucket: StickyBucket, taskId: string): boolean {
  return bucket.taskMeta.has(taskId)
}

/**
 * Resolution priority (when parent id known):
 * 1. trustworthy workUnitKey + parent + backend → work_unit
 * 2. childConversationId → parent_child
 * 3. taskId only → task (with parent) or task_only (without parent)
 *
 * Never key by bare workUnitKey without parent+backend.
 */
export function resolveStickyIdentity(input: {
  backendCacheKey: string
  parentConversationId?: number | null
  childConversationId?: number | null
  workUnitKey?: string | null
  taskId?: string | null
}): StickyKeyResult | null {
  const backendCacheKey = input.backendCacheKey
  if (!backendCacheKey) return null

  const parentConversationId = input.parentConversationId
  const hasParent =
    parentConversationId != null && Number.isFinite(parentConversationId)

  const workUnitKey =
    typeof input.workUnitKey === "string" && input.workUnitKey.length > 0
      ? input.workUnitKey
      : null

  if (hasParent && workUnitKey) {
    return {
      backendCacheKey,
      parentConversationId: parentConversationId as number,
      unit: { kind: "work_unit", workUnitKey },
    }
  }

  const childConversationId = input.childConversationId
  if (
    hasParent &&
    childConversationId != null &&
    Number.isFinite(childConversationId)
  ) {
    return {
      backendCacheKey,
      parentConversationId: parentConversationId as number,
      unit: { kind: "parent_child", childConversationId },
    }
  }

  const taskId =
    typeof input.taskId === "string" && input.taskId.length > 0
      ? input.taskId
      : null

  if (hasParent && taskId) {
    return {
      backendCacheKey,
      parentConversationId: parentConversationId as number,
      unit: { kind: "task", taskId },
    }
  }

  if (taskId) {
    return { kind: "task_only", backendCacheKey, taskId }
  }

  return null
}

/** Canonical map key. Alias name for design’s stickyKeyToString. */
export function stickyIdentityToString(id: StickyKeyResult): string {
  if ("kind" in id && id.kind === "task_only") {
    return `${id.backendCacheKey}|task_only|${id.taskId}`
  }
  const sticky = id as StickyIdentity
  const unitPart =
    sticky.unit.kind === "work_unit"
      ? `work_unit|${sticky.unit.workUnitKey}`
      : sticky.unit.kind === "parent_child"
        ? `parent_child|${sticky.unit.childConversationId}`
        : `task|${sticky.unit.taskId}`
  return `${sticky.backendCacheKey}|${sticky.parentConversationId}|${unitPart}`
}

export const stickyKeyToString = stickyIdentityToString

export function hasPositiveRecovery(s: RecoverySignals): boolean {
  return (
    s.liveBindingRunning ||
    s.childProjectionRunning ||
    s.activeRunNonTerminal ||
    s.openAttention ||
    s.parentWaitingForThisChild ||
    s.continueOrReplaceAdmitted
  )
}

export function foldToolCount(
  state: { peakByTaskId: ReadonlyMap<string, number> },
  taskId: string,
  count: number
): { peakByTaskId: Map<string, number>; display: number } {
  const peakByTaskId = clonePeaks(state.peakByTaskId)
  if (!isFiniteNonNegativeInteger(count)) {
    return { peakByTaskId, display: sumPeaks(peakByTaskId) }
  }
  const prev = peakByTaskId.get(taskId) ?? 0
  peakByTaskId.set(taskId, Math.max(prev, count))
  return { peakByTaskId, display: sumPeaks(peakByTaskId) }
}

function mergeTaskMeta(
  prev: TaskMeta | undefined,
  obs: StickyObservation,
  admissionOrder: number
): TaskMeta {
  const startedAtMs = parseTimestampMs(obs.startedAt)
  const finishedAtMs = parseTimestampMs(obs.finishedAt)
  return {
    generation:
      obs.generation !== undefined && obs.generation !== null
        ? obs.generation
        : (prev?.generation ?? null),
    startedAtMs:
      startedAtMs != null
        ? prev?.startedAtMs != null
          ? Math.min(prev.startedAtMs, startedAtMs)
          : startedAtMs
        : (prev?.startedAtMs ?? null),
    finishedAtMs:
      finishedAtMs != null ? finishedAtMs : (prev?.finishedAtMs ?? null),
    parentToolUseId:
      obs.parentToolUseId !== undefined && obs.parentToolUseId !== null
        ? obs.parentToolUseId
        : (prev?.parentToolUseId ?? null),
    admissionOrder: prev?.admissionOrder ?? admissionOrder,
  }
}

/**
 * Whether this observation targets a run older than the currently admitted
 * active run (stale terminal fence).
 *
 * Ordering: generation when both sides known; else admission order from
 * running-only lineage.
 */
function isStaleRelativeToActive(
  bucket: StickyBucket,
  obs: StickyObservation
): boolean {
  if (bucket.activeTaskId == null) return false
  if (obs.taskId === bucket.activeTaskId) return false

  if (obs.generation != null && bucket.activeGeneration != null) {
    return obs.generation < bucket.activeGeneration
  }

  const obsMeta = bucket.taskMeta.get(obs.taskId)
  const activeMeta = bucket.taskMeta.get(bucket.activeTaskId)
  if (obsMeta != null && activeMeta != null) {
    return obsMeta.admissionOrder < activeMeta.admissionOrder
  }

  // Different task without comparable order: do not let terminal kill active.
  return (
    obs.type === "canceled" || obs.type === "completed" || obs.type === "failed"
  )
}

/**
 * Whether a running observation should update active* fields.
 */
function shouldAdmitRunningAsActive(
  bucket: StickyBucket,
  obs: StickyObservation
): boolean {
  if (bucket.activeTaskId == null) return true

  if (obs.generation != null && bucket.activeGeneration != null) {
    return obs.generation >= bucket.activeGeneration
  }

  if (obs.generation != null && bucket.activeGeneration == null) {
    return true
  }

  // Null / mixed generations: newer running on this unit becomes active
  // (continue/replace, re-enter after terminal, same-task refresh).
  return true
}

function applyToolCount(
  bucket: StickyBucket,
  taskId: string,
  count: number | null | undefined
): StickyBucket {
  if (count == null) return bucket
  if (!isAdmitted(bucket, taskId)) return bucket
  const folded = foldToolCount(bucket, taskId, count)
  return {
    ...bucket,
    peakByTaskId: folded.peakByTaskId,
    lastDisplayToolCount: folded.display,
  }
}

function maybeMoveAnchorEarlier(
  bucket: StickyBucket,
  startedAt: string | null | undefined
): StickyBucket {
  const startedAtMs = parseTimestampMs(startedAt)
  if (startedAtMs == null) return bucket
  if (bucket.anchorStartedAtMs == null) {
    return { ...bucket, anchorStartedAtMs: startedAtMs }
  }
  if (startedAtMs < bucket.anchorStartedAtMs) {
    return { ...bucket, anchorStartedAtMs: startedAtMs }
  }
  return bucket
}

function freezeTerminalElapsed(
  bucket: StickyBucket,
  finishedAt: string | null | undefined,
  nowMs: number
): number | null {
  if (bucket.anchorStartedAtMs == null) {
    return bucket.terminalElapsedMs
  }
  const finishedAtMs = parseTimestampMs(finishedAt)
  if (finishedAtMs != null) {
    const elapsed = finishedAtMs - bucket.anchorStartedAtMs
    return elapsed >= 0 ? elapsed : bucket.terminalElapsedMs
  }
  const elapsed = nowMs - bucket.anchorStartedAtMs
  return elapsed >= 0 ? elapsed : bucket.terminalElapsedMs
}

function classifyCanceledPhase(
  errorCode: string | null | undefined,
  cancelReason: string | null | undefined,
  recovery: RecoverySignals
): StickyPhase {
  const code = errorCode?.trim() ?? ""
  const reason = cancelReason?.trim().toLowerCase() ?? ""

  if (ALWAYS_TERMINAL_ERROR_CODES.has(code)) {
    return "terminal"
  }
  if (ALWAYS_TERMINAL_CANCEL_REASONS.has(reason)) {
    return "terminal"
  }
  if (code === "cancel_delegation" || reason === "cancel_delegation") {
    return "terminal"
  }

  if (RECOVERY_GATED_ERROR_CODES.has(code)) {
    return hasPositiveRecovery(recovery) ? "active_sticky" : "terminal"
  }

  // Bare cancel / unknown orchestration cancel: terminal unless recovery-owned
  // and product maps as intermediate — V1 defaults to terminal without recovery.
  if (code === "" && reason === "") {
    return hasPositiveRecovery(recovery) ? "active_sticky" : "terminal"
  }

  // Business / other failures → terminal
  return "terminal"
}

function applyOrphanClock(
  bucket: StickyBucket,
  recovery: RecoverySignals,
  nowMs: number
): StickyBucket {
  if (bucket.phase !== "active_sticky") {
    return { ...bucket, orphanStartedAtMs: null }
  }

  if (hasPositiveRecovery(recovery)) {
    return { ...bucket, orphanStartedAtMs: null }
  }

  const orphanStartedAtMs =
    bucket.orphanStartedAtMs != null ? bucket.orphanStartedAtMs : nowMs

  if (nowMs - orphanStartedAtMs >= STICKY_ORPHAN_TIMEOUT_MS) {
    return {
      ...bucket,
      phase: "terminal",
      orphanStartedAtMs: null,
      terminalElapsedMs: freezeTerminalElapsed(bucket, null, nowMs),
    }
  }

  return { ...bucket, orphanStartedAtMs }
}

function cloneBucket(bucket: StickyBucket, identityKey: string): StickyBucket {
  return {
    ...bucket,
    identityKey,
    peakByTaskId: clonePeaks(bucket.peakByTaskId),
    taskMeta: cloneTaskMeta(bucket.taskMeta),
  }
}

/**
 * Admit a task into the unit lineage via a `running` observation.
 * Returns updated bucket; first admission assigns monotonic order.
 */
function admitRunningTask(
  bucket: StickyBucket,
  obs: StickyObservation
): StickyBucket {
  const prev = bucket.taskMeta.get(obs.taskId)
  let nextAdmissionOrder = bucket.nextAdmissionOrder
  let admissionOrder = prev?.admissionOrder
  if (admissionOrder == null) {
    admissionOrder = nextAdmissionOrder
    nextAdmissionOrder += 1
  }
  const taskMeta = cloneTaskMeta(bucket.taskMeta)
  taskMeta.set(obs.taskId, mergeTaskMeta(prev, obs, admissionOrder))
  return {
    ...bucket,
    taskMeta,
    nextAdmissionOrder,
  }
}

/**
 * Update meta for an already-admitted task. No-ops if not admitted.
 */
function updateAdmittedTaskMeta(
  bucket: StickyBucket,
  obs: StickyObservation
): StickyBucket {
  const prev = bucket.taskMeta.get(obs.taskId)
  if (prev == null) return bucket
  const taskMeta = cloneTaskMeta(bucket.taskMeta)
  taskMeta.set(obs.taskId, mergeTaskMeta(prev, obs, prev.admissionOrder))
  return { ...bucket, taskMeta }
}

/**
 * Pure sticky observation reducer.
 *
 * Returns `null` when there is no bucket and the observation is not `running`
 * — only a start/running observation may create `(none) → active_sticky`.
 */
export function applyStickyObservation(
  bucket: StickyBucket | null,
  identityKey: string,
  obs: StickyObservation
): StickyBucket | null {
  if (bucket == null && obs.type !== "running") {
    return null
  }

  let next: StickyBucket = bucket
    ? cloneBucket(bucket, identityKey)
    : emptyBucket(identityKey)

  switch (obs.type) {
    case "running": {
      next = admitRunningTask(next, obs)
      next = applyToolCount(next, obs.taskId, obs.toolCallCount)
      next = maybeMoveAnchorEarlier(next, obs.startedAt)

      if (shouldAdmitRunningAsActive(next, obs)) {
        const switchingTask = next.activeTaskId !== obs.taskId
        // Active metadata belongs atomically to the newly admitted task:
        // do not retain a prior task's generation/parentToolUseId.
        next = {
          ...next,
          phase: "active_sticky",
          activeTaskId: obs.taskId,
          activeParentToolUseId: switchingTask
            ? (obs.parentToolUseId ?? null)
            : obs.parentToolUseId !== undefined && obs.parentToolUseId !== null
              ? obs.parentToolUseId
              : next.activeParentToolUseId,
          activeGeneration: switchingTask
            ? (obs.generation ?? null)
            : obs.generation !== undefined && obs.generation !== null
              ? obs.generation
              : next.activeGeneration,
          terminalElapsedMs: null,
        }
      }
      break
    }

    case "stats": {
      if (isAdmitted(next, obs.taskId)) {
        next = updateAdmittedTaskMeta(next, obs)
        next = applyToolCount(next, obs.taskId, obs.toolCallCount)
        next = maybeMoveAnchorEarlier(next, obs.startedAt)
      }
      // Unknown taskIds: no peaks, meta, or anchor moves.
      break
    }

    case "reseed": {
      // Unit-level re-seed hole; keep last display while recovery-owned.
      if (hasPositiveRecovery(obs.recovery)) {
        next = {
          ...next,
          phase: "active_sticky",
        }
      }
      break
    }

    case "canceled": {
      if (isAdmitted(next, obs.taskId)) {
        next = updateAdmittedTaskMeta(next, obs)
        next = applyToolCount(next, obs.taskId, obs.toolCallCount)
        if (!isStaleRelativeToActive(next, obs)) {
          const phase = classifyCanceledPhase(
            obs.errorCode,
            obs.cancelReason,
            obs.recovery
          )
          if (phase === "terminal") {
            next = {
              ...next,
              phase: "terminal",
              orphanStartedAtMs: null,
              terminalElapsedMs: freezeTerminalElapsed(
                next,
                obs.finishedAt,
                obs.nowMs
              ),
            }
          } else {
            next = { ...next, phase: "active_sticky" }
          }
        }
      }
      break
    }

    case "completed":
    case "failed": {
      if (isAdmitted(next, obs.taskId)) {
        next = updateAdmittedTaskMeta(next, obs)
        next = applyToolCount(next, obs.taskId, obs.toolCallCount)
        if (!isStaleRelativeToActive(next, obs)) {
          next = {
            ...next,
            phase: "terminal",
            orphanStartedAtMs: null,
            terminalElapsedMs: freezeTerminalElapsed(
              next,
              obs.finishedAt,
              obs.nowMs
            ),
          }
        }
      }
      break
    }

    case "tick": {
      // Orphan evaluation below handles clock start/fire.
      break
    }
  }

  next = applyOrphanClock(next, obs.recovery, obs.nowMs)
  return next
}

/**
 * Pure latest-only eligibility for a mounted card.
 * Rule (first match wins):
 * 1. generation vs activeGeneration
 * 2. taskId vs activeTaskId
 * 3. parentToolUseId vs activeParentToolUseId
 * 4. else false
 */
export function isLatestStickyCard(
  card: {
    taskId?: string | null
    parentToolUseId?: string | null
    generation?: number | null
  },
  bucket: StickyBucket
): boolean {
  if (card.generation != null && bucket.activeGeneration != null) {
    return card.generation === bucket.activeGeneration
  }
  if (card.taskId && bucket.activeTaskId) {
    return card.taskId === bucket.activeTaskId
  }
  if (card.parentToolUseId && bucket.activeParentToolUseId) {
    return card.parentToolUseId === bucket.activeParentToolUseId
  }
  return false
}
