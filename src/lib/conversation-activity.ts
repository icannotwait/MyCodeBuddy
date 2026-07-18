import type { DbConversationSummary } from "@/lib/types"

export interface OptimisticConversationActivity {
  token: string
  baselineUpdatedAt: string
  effectiveAt: string
}

export type OptimisticActivityById = ReadonlyMap<
  number,
  OptimisticConversationActivity
>

export const EMPTY_OPTIMISTIC_ACTIVITY_BY_ID: OptimisticActivityById = new Map()

export function parseActivityTimestamp(
  value: string | null | undefined
): number {
  if (!value) return 0
  const parsed = Date.parse(value)
  return Number.isNaN(parsed) ? 0 : parsed
}

export function getEffectiveConversationUpdatedAt(
  summary: DbConversationSummary,
  optimisticActivityById: OptimisticActivityById
): string {
  const optimistic = optimisticActivityById.get(summary.id)?.effectiveAt
  if (!optimistic) return summary.updated_at
  return parseActivityTimestamp(optimistic) >
    parseActivityTimestamp(summary.updated_at)
    ? optimistic
    : summary.updated_at
}

export function nextOptimisticActivityTimestamp(
  baselineUpdatedAt: string,
  previousEffectiveMs: number,
  nowMs = Date.now()
): { effectiveAt: string; effectiveMs: number } {
  const effectiveMs = Math.max(
    nowMs,
    previousEffectiveMs + 1,
    parseActivityTimestamp(baselineUpdatedAt) + 1
  )
  return { effectiveAt: new Date(effectiveMs).toISOString(), effectiveMs }
}
