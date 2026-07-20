/**
 * Cross-module bridge so pop-out orchestration (outside React) can fence
 * main-window ACP teardown and release ownership without acpDisconnect.
 *
 * Each webview has its own React ACP context; `AcpConnectionsProvider`
 * registers the release implementation on mount.
 */

export type PopoutTransferFence = {
  conversationId: number
  operationId: string
  /** True after main dropped local owner UI without acpDisconnect. */
  mainReleased: boolean
}

export type PopoutAcpBridge = {
  /**
   * Drop local owner UI for this conversation without calling acpDisconnect.
   * Must be synchronous or return a promise the orchestrator awaits.
   */
  releaseConnectionWithoutDisconnect: (
    conversationId: number,
    operationId: string
  ) => void | Promise<void>
  reclaimAfterAbort?: (
    conversationId: number,
    operationId: string
  ) => void | Promise<void>
}

const fences = new Map<number, PopoutTransferFence>()
let bridge: PopoutAcpBridge | null = null

export function registerPopoutAcpBridge(next: PopoutAcpBridge | null): void {
  bridge = next
}

export function getPopoutAcpBridge(): PopoutAcpBridge | null {
  return bridge
}

export function markTransferringOut(
  conversationId: number,
  operationId: string
): void {
  if (conversationId <= 0 || !operationId) return
  fences.set(conversationId, {
    conversationId,
    operationId,
    mainReleased: false,
  })
}

/** Compare-and-clear: only clears if fence.operationId matches. */
export function clearTransferringOut(
  conversationId: number,
  operationId: string
): void {
  const cur = fences.get(conversationId)
  if (cur && cur.operationId === operationId) {
    fences.delete(conversationId)
  }
}

export function markMainReleased(
  conversationId: number,
  operationId: string
): void {
  const cur = fences.get(conversationId)
  if (cur && cur.operationId === operationId) {
    fences.set(conversationId, { ...cur, mainReleased: true })
  }
}

export function getTransferFence(
  conversationId: number
): PopoutTransferFence | null {
  return fences.get(conversationId) ?? null
}

/** True when main must not acpDisconnect this conversation (transfer in flight). */
export function isTransferringOut(
  conversationId: number | null | undefined
): boolean {
  if (conversationId == null || conversationId <= 0) return false
  return fences.has(conversationId)
}

/**
 * Await main provider release (drop local owner UI, no acpDisconnect).
 * Falls back to marking mainReleased if no bridge is registered (tests).
 */
export async function releaseConnectionWithoutDisconnect(
  conversationId: number,
  operationId: string
): Promise<void> {
  markMainReleased(conversationId, operationId)
  const impl = bridge?.releaseConnectionWithoutDisconnect
  if (impl) {
    await impl(conversationId, operationId)
  }
}

/** Test helper */
export function __resetTransferFencesForTests(): void {
  fences.clear()
  bridge = null
}
