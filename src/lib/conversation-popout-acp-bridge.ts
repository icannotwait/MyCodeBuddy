/**
 * Cross-module bridge so pop-out orchestration (outside React) can fence
 * main-window ACP teardown and release ownership without acpDisconnect.
 *
 * Each webview has its own React ACP context; `AcpConnectionsProvider`
 * registers the release / claim implementation on mount.
 */

import type { AgentType } from "@/lib/types"

export type PopoutTransferFence = {
  conversationId: number
  operationId: string
  /** True after main dropped local owner UI without acpDisconnect. */
  mainReleased: boolean
}

export type ClaimConnectionOwnershipArgs = {
  conversationId: number
  connectionId?: string | null
  agentType: AgentType
  workingDir: string
  operationId: string
  contextKey: string
  expectedOwnerWindowLabel?: string
  /** Incarnation generation from forward rebind; stored on local owner entry. */
  ownershipGeneration?: number | null
  /** Detached owner window label (e.g. conversation-12). */
  ownerWindowLabel?: string | null
}

export type ClaimConnectionOwnershipResult = {
  ownershipGeneration?: number
  connectionId?: string
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
  /**
   * Detached: attach as owner UI for a live connection (after rebind), or
   * no-op for cold (connectionId null). Must not spawn a second agent.
   */
  claimConnectionOwnership?: (
    args: ClaimConnectionOwnershipArgs
  ) => Promise<ClaimConnectionOwnershipResult | void>
}

const fences = new Map<number, PopoutTransferFence>()
/** Detached: suppress acpDisconnect until commit-ack (transfer lifetime). */
const suppressFrontendDisconnect = new Set<number>()
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
 * Detached transfer lifetime: when true, frontend disconnect must not
 * acpDisconnect (viewer-style detach only). Cleared after commit-ack.
 */
export function setSuppressFrontendDisconnect(
  conversationId: number,
  suppress: boolean
): void {
  if (conversationId <= 0) return
  if (suppress) suppressFrontendDisconnect.add(conversationId)
  else suppressFrontendDisconnect.delete(conversationId)
}

export function isFrontendDisconnectSuppressed(
  conversationId: number | null | undefined
): boolean {
  if (conversationId == null || conversationId <= 0) return false
  return suppressFrontendDisconnect.has(conversationId)
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

/**
 * Detached claim: requires a registered bridge (provider mounted).
 * Live paths must not treat a missing bridge as success.
 */
export async function claimConnectionOwnership(
  args: ClaimConnectionOwnershipArgs
): Promise<ClaimConnectionOwnershipResult> {
  const impl = bridge?.claimConnectionOwnership
  if (!impl) {
    throw new Error("ACP ownership claim bridge is not registered")
  }
  const result = await impl(args)
  return result ?? {}
}

/** Build lease args for acpDisconnect when connection has pop-out ownership. */
export function leaseArgsForDisconnect(conn: {
  ownershipGeneration?: number | null
  ownerOperationId?: string | null
  ownerWindowLabel?: string | null
}): {
  expectedOwnerWindow?: string | null
  expectedOperationId?: string | null
  expectedOwnershipGeneration?: number | null
} | null {
  const hasLease =
    (conn.ownerOperationId != null && conn.ownerOperationId !== "") ||
    (conn.ownerWindowLabel != null && conn.ownerWindowLabel !== "") ||
    conn.ownershipGeneration != null
  if (!hasLease) return null
  return {
    expectedOwnerWindow: conn.ownerWindowLabel ?? null,
    expectedOperationId: conn.ownerOperationId ?? null,
    expectedOwnershipGeneration: conn.ownershipGeneration ?? null,
  }
}

/** Test helper */
export function __resetTransferFencesForTests(): void {
  fences.clear()
  suppressFrontendDisconnect.clear()
  bridge = null
}
