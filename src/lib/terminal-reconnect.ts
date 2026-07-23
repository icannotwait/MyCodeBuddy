import type { DbConversationSummary, EventEnvelope } from "@/lib/types"

/** Captured root-summary `updated_at` when a terminal disconnect is latched. */
export interface TerminalDisconnectLatch {
  baselineUpdatedAt: string
}

/**
 * Whether a live event should arm the terminal-disconnect latch for this tab.
 *
 * True only when the event belongs to the same connection, the persisted root
 * summary is still `in_progress`, and the event is either a terminal error or a
 * bare disconnected status change.
 */
export function shouldLatchTerminalDisconnect(
  event: EventEnvelope,
  connectionId: string | null,
  summary: Pick<DbConversationSummary, "status" | "updated_at"> | null
): boolean {
  if (
    connectionId == null ||
    event.connection_id !== connectionId ||
    summary?.status !== "in_progress"
  ) {
    return false
  }
  return event.type === "error"
    ? event.terminal
    : event.type === "status_changed" && event.status === "disconnected"
}

/**
 * Whether an armed terminal latch should clear given the latest root summary.
 *
 * Only a non-`cancelled` summary whose `updated_at` is strictly newer than the
 * latch baseline clears. Stale same-timestamp `in_progress` and newer
 * `cancelled` rows leave the latch armed.
 */
export function shouldClearTerminalDisconnectLatch(
  latch: TerminalDisconnectLatch | null,
  summary: Pick<DbConversationSummary, "status" | "updated_at"> | null
): boolean {
  return (
    latch != null &&
    summary != null &&
    summary.status !== "cancelled" &&
    Date.parse(summary.updated_at) > Date.parse(latch.baselineUpdatedAt)
  )
}
