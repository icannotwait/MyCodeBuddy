/**
 * One-shot intent for opening a delegated child conversation as a main tab.
 *
 * Replaces the former SubAgentSessionDialog props:
 * - live ownership + kickoff text for viewer-safe transcript promotion
 * - optional turn anchor so a selected run focuses the matching message
 *
 * Intents are local/memory-only (not tab persistence / remote sync).
 * `consume` is single-use so reopening the same tab later does not re-fire.
 */

export type DelegatedChildTabIntent = {
  focusTurnAnchor: string | null
  kickoffTask: string | null
  /** When true, session surface should mark liveOwnsActiveTurn on open. */
  liveOwnsActiveTurn: boolean
}

const intents = new Map<number, DelegatedChildTabIntent>()
let version = 0
const listeners = new Set<() => void>()

function bump(): void {
  version += 1
  for (const l of listeners) l()
}

export function setDelegatedChildTabIntent(
  conversationId: number,
  intent: DelegatedChildTabIntent
): void {
  if (conversationId <= 0) return
  intents.set(conversationId, {
    focusTurnAnchor: intent.focusTurnAnchor ?? null,
    kickoffTask: intent.kickoffTask ?? null,
    liveOwnsActiveTurn: intent.liveOwnsActiveTurn,
  })
  bump()
}

/** Non-destructive read (tests / debug). */
export function peekDelegatedChildTabIntent(
  conversationId: number
): DelegatedChildTabIntent | null {
  return intents.get(conversationId) ?? null
}

/** Consume intent for a child conversation (one-shot). */
export function consumeDelegatedChildTabIntent(
  conversationId: number
): DelegatedChildTabIntent | null {
  const intent = intents.get(conversationId) ?? null
  if (intent) {
    intents.delete(conversationId)
    bump()
  }
  return intent
}

export function clearDelegatedChildTabIntent(conversationId: number): void {
  if (intents.delete(conversationId)) bump()
}

export function resetDelegatedChildTabIntents(): void {
  if (intents.size === 0) return
  intents.clear()
  bump()
}

export function getDelegatedChildTabIntentVersion(): number {
  return version
}

export function subscribeDelegatedChildTabIntents(
  listener: () => void
): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}
