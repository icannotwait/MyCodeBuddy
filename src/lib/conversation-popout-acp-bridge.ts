/**
 * Cross-module bridge so pop-out orchestration (outside React) can fence
 * main-window ACP teardown without calling context methods directly.
 *
 * Each webview has its own React ACP context; pure `popOutConversation`
 * registers transfer fences here, and `AcpConnectionsProvider` consults them
 * before idle-sweep / unmount disconnect.
 */

export type PopoutTransferFence = {
  conversationId: number
  operationId: string
  /** True after main dropped local owner UI without acpDisconnect. */
  mainReleased: boolean
}

const fences = new Map<number, PopoutTransferFence>()

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
export function isTransferringOut(conversationId: number | null | undefined): boolean {
  if (conversationId == null || conversationId <= 0) return false
  return fences.has(conversationId)
}

/** Test helper */
export function __resetTransferFencesForTests(): void {
  fences.clear()
}
