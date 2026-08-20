import {
  type RequestUsageSnapshot,
  EMPTY_REQUEST_USAGE,
} from "@/lib/request-usage-speed"

const snaps = new Map<number, RequestUsageSnapshot>()
const canonicalOf = new Map<number, number>()
const groupOf = new Map<number, Set<number>>()
const listeners = new Set<() => void>()

function notify(): void {
  listeners.forEach((l) => l())
}

function groupFor(id: number): Set<number> {
  const canonical = canonicalOf.get(id) ?? id
  const existing = groupOf.get(canonical)
  if (existing) return existing
  const created = new Set([id])
  canonicalOf.set(id, canonical)
  groupOf.set(canonical, created)
  return created
}

/** Draft/runtime ids and the bound DB row share one live usage snapshot. */
export function aliasRequestUsageIds(fromId: number, toId: number): void {
  if (fromId === 0 || toId === 0 || fromId === toId) return
  const fromCanon = canonicalOf.get(fromId) ?? fromId
  const toCanon = canonicalOf.get(toId) ?? toId
  const fromGroup = groupFor(fromId)
  const toGroup = groupFor(toId)
  if (fromCanon === toCanon) return

  const merged = new Set<number>([...fromGroup, ...toGroup, fromId, toId])
  const fromSnap = snaps.get(fromCanon)
  const toSnap = snaps.get(toCanon)
  const snap =
    fromSnap && (!toSnap || fromSnap.sampleCount >= toSnap.sampleCount)
      ? fromSnap
      : (toSnap ?? EMPTY_REQUEST_USAGE)

  groupOf.delete(fromCanon)
  groupOf.delete(toCanon)
  for (const id of merged) {
    canonicalOf.set(id, toId)
    snaps.set(id, snap)
  }
  groupOf.set(toId, merged)
  notify()
}

export function publishRequestUsage(
  conversationId: number | null | undefined,
  snap: RequestUsageSnapshot
): void {
  if (conversationId == null || conversationId === 0) return
  const group = groupFor(conversationId)
  for (const id of group) {
    snaps.set(id, snap)
  }
  notify()
}

export function getPublishedRequestUsage(
  conversationId: number
): RequestUsageSnapshot {
  const canonical = canonicalOf.get(conversationId) ?? conversationId
  return (
    snaps.get(canonical) ?? snaps.get(conversationId) ?? EMPTY_REQUEST_USAGE
  )
}

export function subscribeRequestUsage(listener: () => void): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}
