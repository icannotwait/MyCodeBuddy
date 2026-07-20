/**
 * Backend-scoped cache of cold child-conversation projections for delegation
 * cards (title + durable delegation summary fields).
 *
 * Keys are `(getActiveBackendCacheKey(), childConversationId)`. Fetch is via
 * `getFolderConversation` once per miss (summary-only map). Install site for
 * conversation-change + reconnect is `AppWorkspaceProvider` only.
 */

import { getFolderConversation } from "@/lib/api"
import { getActiveBackendCacheKey } from "@/lib/transport"
import type {
  AttentionRequestSummary,
  ConversationChange,
  DbConversationDetail,
  DbConversationSummary,
  DelegationRuntimeStats,
} from "@/lib/types"

const DEFAULT_MAX_SOFT_ENTRIES = 64
const DEFAULT_MAX_CONCURRENT = 4
const TOMBSTONE_CAP = 512

export type ChildTaskStatus =
  | "running"
  | "completed"
  | "failed"
  | "canceled"
  | null

export type ChildCardProjection = {
  childConversationId: number
  title: string | null
  /** Durable broker task id when known (`summary.delegation_call_id`). */
  taskId: string | null
  /** Normalized from `delegation_task_status`. */
  taskStatus: ChildTaskStatus
  errorCode: string | null
  startedAt: string | null
  finishedAt: string | null
  runtimeStats: DelegationRuntimeStats | null
  attentionRequest: AttentionRequestSummary | null
  /** True when taskStatus is completed|failed|canceled. */
  isTerminal: boolean
}

export interface DelegationChildProjectionCacheOptions {
  fetchConversation?: (id: number) => Promise<DbConversationDetail>
  getBackendKey?: () => string
  maxSoftEntries?: number
  maxConcurrent?: number
}

type CacheKey = string

interface Entry {
  projection: ChildCardProjection | null
  generation: number
  interest: number
  /** Soft-LRU recency; only meaningful when interest === 0 and projection set. */
  softTick: number
}

function encodeKey(backend: string, id: number): CacheKey {
  return `${backend}\0${id}`
}

function isTerminalStatus(status: Exclude<ChildTaskStatus, null>): boolean {
  return status === "completed" || status === "failed" || status === "canceled"
}

/** Normalize wire `delegation_task_status` to the locked card contract. */
export function normalizeChildTaskStatus(
  raw: string | null | undefined
): ChildTaskStatus {
  if (raw == null || raw === "") return null
  switch (raw) {
    case "running":
    case "completed":
    case "failed":
    case "canceled":
      return raw
    case "cancelled":
      // British spelling occasionally seen in older rows.
      return "canceled"
    default:
      return null
  }
}

export function mapSummaryToChildCardProjection(
  summary: DbConversationSummary
): ChildCardProjection {
  const taskStatus = normalizeChildTaskStatus(summary.delegation_task_status)
  return {
    childConversationId: summary.id,
    title: summary.title ?? null,
    taskId: summary.delegation_call_id ?? null,
    taskStatus,
    errorCode: summary.delegation_error_code ?? null,
    startedAt: summary.delegation_started_at ?? null,
    finishedAt: summary.delegation_finished_at ?? null,
    runtimeStats: summary.delegation_runtime_stats ?? null,
    attentionRequest: summary.delegation_attention_request ?? null,
    isTerminal: taskStatus != null && isTerminalStatus(taskStatus),
  }
}

export class DelegationChildProjectionCache {
  private readonly fetchConversation: (
    id: number
  ) => Promise<DbConversationDetail>
  private readonly getBackendKey: () => string
  private readonly maxSoftEntries: number
  private readonly maxConcurrent: number

  private readonly entries = new Map<CacheKey, Entry>()
  private readonly inFlight = new Map<CacheKey, Promise<void>>()
  private readonly tombstones = new Map<CacheKey, true>()
  private readonly listeners = new Set<() => void>()
  private readonly waitQueue: Array<() => void> = []

  private softTick = 0
  private activeFetches = 0

  constructor(options: DelegationChildProjectionCacheOptions = {}) {
    this.fetchConversation =
      options.fetchConversation ?? ((id) => getFolderConversation(id))
    this.getBackendKey = options.getBackendKey ?? getActiveBackendCacheKey
    this.maxSoftEntries = options.maxSoftEntries ?? DEFAULT_MAX_SOFT_ENTRIES
    this.maxConcurrent = options.maxConcurrent ?? DEFAULT_MAX_CONCURRENT
  }

  get(childConversationId: number): ChildCardProjection | null {
    const key = this.keyFor(childConversationId)
    if (this.tombstones.has(key)) return null
    const entry = this.entries.get(key)
    if (!entry?.projection) return null
    if (entry.interest === 0) {
      entry.softTick = ++this.softTick
    }
    return entry.projection
  }

  /**
   * Ensure a projection is loading or present for `childConversationId`.
   * Concurrent ensures share one in-flight promise (no generation bump).
   */
  ensure(childConversationId: number): void {
    const key = this.keyFor(childConversationId)
    if (this.tombstones.has(key)) return
    const entry = this.entries.get(key)
    if (entry?.projection) return
    if (this.inFlight.has(key)) return
    this.startFetch(childConversationId, key, /*force*/ false)
  }

  /**
   * Retain interest in an id (mounted card). Returns a release callback.
   * When interest hits 0 the entry becomes a soft LRU candidate (cap
   * `maxSoftEntries`) rather than dropping immediately.
   */
  retain(childConversationId: number): () => void {
    const key = this.keyFor(childConversationId)
    let entry = this.entries.get(key)
    if (!entry) {
      entry = {
        projection: null,
        generation: 0,
        interest: 0,
        softTick: 0,
      }
      this.entries.set(key, entry)
    }
    entry.interest += 1
    let released = false
    return () => {
      if (released) return
      released = true
      const current = this.entries.get(key)
      if (!current) return
      current.interest = Math.max(0, current.interest - 1)
      if (current.interest === 0) {
        if (current.projection) {
          current.softTick = ++this.softTick
          this.evictSoftOverflow()
        } else if (!this.inFlight.has(key)) {
          // No data and no flight — drop the interest placeholder.
          this.entries.delete(key)
        }
      }
    }
  }

  applyConversationChange(change: ConversationChange): void {
    if (change.kind === "state") {
      // State patches only carry status/token/updated_at — not projection fields.
      return
    }
    if (change.kind === "deleted") {
      this.applyDelete(change.id)
      return
    }
    this.applyUpsert(change.summary)
  }

  /**
   * Force-refetch every interest-held id for the active backend (reconnect
   * backstop). Soft-only entries are left alone.
   */
  refetchTracked(): void {
    const backend = this.getBackendKey()
    const ids: number[] = []
    for (const [key, entry] of this.entries) {
      if (entry.interest <= 0) continue
      if (!key.startsWith(`${backend}\0`)) continue
      const id = Number(key.slice(backend.length + 1))
      if (!Number.isFinite(id)) continue
      ids.push(id)
    }
    for (const id of ids) {
      const key = encodeKey(backend, id)
      // Drop shared in-flight so a fresh generation can start.
      this.inFlight.delete(key)
      this.startFetch(id, key, /*force*/ true)
    }
  }

  subscribe(cb: () => void): () => void {
    this.listeners.add(cb)
    return () => {
      this.listeners.delete(cb)
    }
  }

  /** Test / backend-reset helper: drop all state. */
  reset(): void {
    this.entries.clear()
    this.inFlight.clear()
    this.tombstones.clear()
    this.waitQueue.length = 0
    this.activeFetches = 0
    this.softTick = 0
  }

  // ── internals ──────────────────────────────────────────────────────────

  private keyFor(id: number): CacheKey {
    return encodeKey(this.getBackendKey(), id)
  }

  private notify(): void {
    for (const cb of this.listeners) {
      try {
        cb()
      } catch {
        // Subscriber errors must not break the cache.
      }
    }
  }

  private applyDelete(id: number): void {
    const key = this.keyFor(id)
    const existing = this.entries.get(key)
    if (existing) {
      existing.generation += 1
      existing.projection = null
    }
    this.entries.delete(key)
    this.inFlight.delete(key)
    this.tombstones.set(key, true)
    while (this.tombstones.size > TOMBSTONE_CAP) {
      const oldest = this.tombstones.keys().next().value
      if (oldest === undefined) break
      this.tombstones.delete(oldest)
    }
    this.notify()
  }

  private applyUpsert(summary: DbConversationSummary): void {
    const key = this.keyFor(summary.id)
    if (this.tombstones.has(key)) return

    let entry = this.entries.get(key)
    // Only hydrate known/tracked ids — do not grow the cache from sidebar noise.
    if (!entry && !this.inFlight.has(key)) return

    if (!entry) {
      entry = {
        projection: null,
        generation: 0,
        interest: 0,
        softTick: 0,
      }
      this.entries.set(key, entry)
    }

    entry.generation += 1
    entry.projection = mapSummaryToChildCardProjection(summary)
    if (entry.interest === 0) {
      entry.softTick = ++this.softTick
      this.evictSoftOverflow()
    }
    this.notify()
  }

  private startFetch(
    childConversationId: number,
    key: CacheKey,
    force: boolean
  ): void {
    if (this.tombstones.has(key)) return
    if (!force && this.inFlight.has(key)) return

    let entry = this.entries.get(key)
    if (!entry) {
      entry = {
        projection: null,
        generation: 0,
        interest: 0,
        softTick: 0,
      }
      this.entries.set(key, entry)
    }
    // Bump generation for every *new* flight so prior late results are ignored.
    entry.generation += 1
    const generation = entry.generation

    const run = async () => {
      try {
        if (this.tombstones.has(key)) return
        const current = this.entries.get(key)
        if (!current || current.generation !== generation) return

        const detail = await this.fetchWithRetry(childConversationId)
        if (this.tombstones.has(key)) return
        const after = this.entries.get(key)
        if (!after || after.generation !== generation) return

        after.projection = mapSummaryToChildCardProjection(detail.summary)
        if (after.interest === 0) {
          after.softTick = ++this.softTick
          this.evictSoftOverflow()
        }
        this.notify()
      } catch {
        // Fetch failed after retry — leave miss; a later ensure/upsert/refetch
        // can recover. Do not wipe a projection that force-refetch was replacing
        // if generation advanced (already handled by generation guard).
      } finally {
        this.releasePermit()
        // Clear in-flight only if we still own this flight.
        if (this.inFlight.get(key) === flight) {
          this.inFlight.delete(key)
        }
        const leftover = this.entries.get(key)
        if (
          leftover &&
          leftover.projection == null &&
          leftover.interest === 0 &&
          !this.inFlight.has(key)
        ) {
          this.entries.delete(key)
        }
      }
    }

    // Acquire permit synchronously when a slot is free so the first
    // `fetchConversation` call is observable in the same turn as `ensure`
    // (tests + concurrent-cap accounting). Otherwise queue and start later.
    const flight = this.withPermit(run)
    this.inFlight.set(key, flight)
  }

  /**
   * Run `fn` under the concurrency permit. When a slot is free, `fn` starts
   * on this turn (so the first await inside is the real fetch). When saturated,
   * `fn` is queued until a permit is released.
   */
  private withPermit(fn: () => Promise<void>): Promise<void> {
    if (this.activeFetches < this.maxConcurrent) {
      this.activeFetches += 1
      return fn()
    }
    return new Promise<void>((resolve) => {
      this.waitQueue.push(() => {
        this.activeFetches += 1
        resolve(fn())
      })
    })
  }

  private async fetchWithRetry(id: number): Promise<DbConversationDetail> {
    try {
      return await this.fetchConversation(id)
    } catch {
      // One transient retry on any rejection (network / 5xx / transport).
      return await this.fetchConversation(id)
    }
  }

  private releasePermit(): void {
    this.activeFetches = Math.max(0, this.activeFetches - 1)
    const next = this.waitQueue.shift()
    if (next) {
      next()
    }
  }

  private evictSoftOverflow(): void {
    // Soft candidates: interest === 0 and have a projection.
    const soft: Array<{ key: CacheKey; tick: number }> = []
    for (const [key, entry] of this.entries) {
      if (entry.interest > 0) continue
      if (!entry.projection) continue
      soft.push({ key, tick: entry.softTick })
    }
    if (soft.length <= this.maxSoftEntries) return
    soft.sort((a, b) => a.tick - b.tick)
    const dropCount = soft.length - this.maxSoftEntries
    for (let i = 0; i < dropCount; i++) {
      const key = soft[i]!.key
      // Never drop an in-flight key mid-fetch (interest 0 but still loading).
      if (this.inFlight.has(key)) continue
      this.entries.delete(key)
    }
  }
}

export const delegationChildProjectionCache =
  new DelegationChildProjectionCache()
