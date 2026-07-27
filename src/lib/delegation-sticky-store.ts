/**
 * External sticky work-unit store for parent delegation cards.
 *
 * Holds `StickyBucket` snapshots behind subscribe/getSnapshot for
 * `useSyncExternalStore`. Mutations enter only via `observeSticky` (effects /
 * handlers) or backend reset — never from pure card model builders.
 *
 * Orphan timeout is driven by a real timer so it fires even when the UI ticker
 * is ineligible (e.g. invalid / missing `startedAt`).
 */

import {
  applyStickyObservation,
  resolveStickyIdentity,
  stickyIdentityToString,
  STICKY_ORPHAN_TIMEOUT_MS,
  type RecoverySignals,
  type StickyBucket,
  type StickyObservation,
} from "@/lib/delegation-sticky-runtime"
import { registerBackendScopedStoreReset } from "@/stores/backend-scoped-store-reset"

/** Max terminal sticky buckets retained (LRU). Active buckets are unbounded. */
export const STICKY_TERMINAL_RETENTION = 200

const EMPTY_RECOVERY: RecoverySignals = {
  liveBindingRunning: false,
  childProjectionRunning: false,
  activeRunNonTerminal: false,
  openAttention: false,
  parentWaitingForThisChild: false,
  continueOrReplaceAdmitted: false,
}

export type ObserveStickyInput = {
  backendCacheKey: string
  parentConversationId?: number | null
  childConversationId?: number | null
  workUnitKey?: string | null
} & StickyObservation

const buckets = new Map<string, StickyBucket>()
const listeners = new Set<() => void>()
const orphanTimers = new Map<string, ReturnType<typeof setTimeout>>()
/** Terminal identity keys in LRU order (oldest first). */
const terminalOrder: string[] = []

function emit(): void {
  for (const listener of listeners) {
    listener()
  }
}

function cancelOrphanTimer(identityKey: string): void {
  const handle = orphanTimers.get(identityKey)
  if (handle != null) {
    clearTimeout(handle)
    orphanTimers.delete(identityKey)
  }
}

function cancelAllOrphanTimers(): void {
  for (const key of [...orphanTimers.keys()]) {
    cancelOrphanTimer(key)
  }
}

/**
 * Schedule / reschedule orphan fire for a bucket. Uses observation `nowMs` for
 * remaining delay so pure-clock semantics stay aligned with the reducer.
 * On fire, evaluates a `tick` at `orphanStartedAtMs + STICKY_ORPHAN_TIMEOUT_MS`
 * so the timeout is deterministic without depending on a valid `startedAt`.
 */
function syncOrphanTimer(
  identityKey: string,
  bucket: StickyBucket,
  nowMs: number
): void {
  cancelOrphanTimer(identityKey)
  if (bucket.phase !== "active_sticky" || bucket.orphanStartedAtMs == null) {
    return
  }
  const elapsed = nowMs - bucket.orphanStartedAtMs
  const remaining = Math.max(0, STICKY_ORPHAN_TIMEOUT_MS - elapsed)
  const handle = setTimeout(() => {
    orphanTimers.delete(identityKey)
    fireOrphanTimeout(identityKey)
  }, remaining)
  orphanTimers.set(identityKey, handle)
}

function fireOrphanTimeout(identityKey: string): void {
  const bucket = buckets.get(identityKey)
  if (
    bucket == null ||
    bucket.phase !== "active_sticky" ||
    bucket.orphanStartedAtMs == null
  ) {
    return
  }
  const nowMs = bucket.orphanStartedAtMs + STICKY_ORPHAN_TIMEOUT_MS
  const next = applyStickyObservation(bucket, identityKey, {
    type: "tick",
    taskId: bucket.activeTaskId ?? "",
    nowMs,
    recovery: EMPTY_RECOVERY,
  })
  if (next == null) return
  commitBucket(identityKey, next, nowMs)
}

function removeFromTerminalOrder(identityKey: string): void {
  const idx = terminalOrder.indexOf(identityKey)
  if (idx >= 0) {
    terminalOrder.splice(idx, 1)
  }
}

function touchTerminalLru(identityKey: string): void {
  removeFromTerminalOrder(identityKey)
  terminalOrder.push(identityKey)
  while (terminalOrder.length > STICKY_TERMINAL_RETENTION) {
    const evictKey = terminalOrder.shift()
    if (evictKey == null) break
    const evictBucket = buckets.get(evictKey)
    if (evictBucket?.phase === "terminal") {
      cancelOrphanTimer(evictKey)
      buckets.delete(evictKey)
    }
  }
}

function updateTerminalRetention(
  identityKey: string,
  bucket: StickyBucket
): void {
  if (bucket.phase === "terminal") {
    touchTerminalLru(identityKey)
  } else {
    removeFromTerminalOrder(identityKey)
  }
}

function commitBucket(
  identityKey: string,
  next: StickyBucket,
  nowMs: number
): void {
  buckets.set(identityKey, next)
  updateTerminalRetention(identityKey, next)
  syncOrphanTimer(identityKey, next, nowMs)
  emit()
}

/**
 * Apply one sticky observation. Returns null when identity cannot be resolved
 * or when the pure reducer refuses to materialize a bucket (first non-running).
 */
export function observeSticky(
  input: ObserveStickyInput
): { identityKey: string; bucket: StickyBucket } | null {
  const resolved = resolveStickyIdentity({
    backendCacheKey: input.backendCacheKey,
    parentConversationId: input.parentConversationId,
    childConversationId: input.childConversationId,
    workUnitKey: input.workUnitKey,
    taskId: input.taskId,
  })
  if (resolved == null) return null

  const identityKey = stickyIdentityToString(resolved)
  const prev = buckets.get(identityKey) ?? null

  const obs: StickyObservation = {
    type: input.type,
    taskId: input.taskId,
    nowMs: input.nowMs,
    generation: input.generation,
    parentToolUseId: input.parentToolUseId,
    startedAt: input.startedAt,
    finishedAt: input.finishedAt,
    toolCallCount: input.toolCallCount,
    errorCode: input.errorCode,
    cancelReason: input.cancelReason,
    recovery: input.recovery,
  }

  const next = applyStickyObservation(prev, identityKey, obs)
  if (next == null) return null

  commitBucket(identityKey, next, input.nowMs)
  return { identityKey, bucket: next }
}

/** Snapshot for `useSyncExternalStore` — same reference when unchanged. */
export function getStickySnapshot(
  identityKey: string
): StickyBucket | undefined {
  return buckets.get(identityKey)
}

export function subscribeSticky(listener: () => void): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

/** Drop all sticky buckets for one backend cache key. */
export function resetStickyBackend(backendCacheKey: string): void {
  if (!backendCacheKey) return
  const prefix = `${backendCacheKey}|`
  let changed = false
  for (const key of [...buckets.keys()]) {
    if (!key.startsWith(prefix)) continue
    cancelOrphanTimer(key)
    buckets.delete(key)
    removeFromTerminalOrder(key)
    changed = true
  }
  if (changed) emit()
}

/** Full clear (backend identity change / test isolation). */
function resetAllSticky(): void {
  if (buckets.size === 0 && orphanTimers.size === 0) {
    terminalOrder.length = 0
    return
  }
  cancelAllOrphanTimers()
  buckets.clear()
  terminalOrder.length = 0
  emit()
}

// Module load: participate in realm backend-identity teardown.
registerBackendScopedStoreReset(resetAllSticky)
