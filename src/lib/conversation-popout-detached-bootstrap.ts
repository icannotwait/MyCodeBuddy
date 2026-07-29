/**
 * Pure bootstrap gates for the detached conversation window.
 * Keeps claim-before-activate / cold-until-commit-ack rules testable
 * without mounting the full React page.
 */

import type { AgentType } from "@/lib/types"

export const CONVERSATION_WINDOW_READY_EVENT = "conversation-window://ready"
export const CONVERSATION_WINDOW_COMMIT_ACK_EVENT =
  "conversation-window://commit-ack"

/**
 * Dispatched inside a detached conversation webview after OS window focus
 * (see Rust `activate_conversation_window` / `REQUEST_COMPOSER_FOCUS_JS`).
 * MessageInput listens and focuses the TipTap composer so the caret is ready
 * when the user re-activates a pop-out from the sidebar.
 */
export const FOCUS_COMPOSER_EVENT = "codeg:focus-composer"

export type ParsedPopoutQuery = {
  conversationId: number
  folderId: number
  agentType: AgentType
  operationId: string
}

const ALLOWED_AGENTS: AgentType[] = [
  "claude_code",
  "codex",
  "open_code",
  "gemini",
  "cline",
  "hermes",
  "code_buddy",
  "kimi_code",
  "pi",
  "grok",
  "cursor",
]

export function parseAgentType(
  raw: string | null | undefined
): AgentType | null {
  if (!raw) return null
  return (ALLOWED_AGENTS as string[]).includes(raw) ? (raw as AgentType) : null
}

export function parseConversationPopoutQuery(args: {
  conversationId: string | null
  folderId: string | null
  agentType: string | null
  operationId: string | null
}): ParsedPopoutQuery | null {
  const conversationId = Number(args.conversationId ?? "0")
  const folderId = Number(args.folderId ?? "0")
  const agentType = parseAgentType(args.agentType)
  const operationId = args.operationId ?? ""

  if (
    !Number.isFinite(conversationId) ||
    conversationId <= 0 ||
    !Number.isFinite(folderId) ||
    folderId <= 0 ||
    !agentType ||
    operationId.length === 0
  ) {
    return null
  }

  return { conversationId, folderId, agentType, operationId }
}

export function conversationWindowLabel(conversationId: number): string {
  return `conversation-${conversationId}`
}

/**
 * Connect / disconnect gate after metadata load.
 *
 * - Live: isActive only after claim succeeds
 * - Cold: isActive false until commit-ack
 * - Before bootstrap ready: never auto-connect
 * - suppressFrontendDisconnect: always true for the detached window lifetime
 *   (post-commit-ack included). Suppress dies with the JS context — never
 *   clear on ack or parent unmount, so teardown cannot bare-acpDisconnect.
 */
export function resolveDetachedConnectGate(args: {
  /** Metadata loaded AND (live path claimed OR cold path ready-to-emit). */
  bootstrapReady: boolean
  /** True when a live backend connection was found and claimed/rebound. */
  isLivePath: boolean
  /** Main completed HandoffComplete and we received ack (or poll confirmed). */
  commitAcked: boolean
}): {
  isActive: boolean
  suppressFrontendDisconnect: boolean
} {
  if (!args.bootstrapReady) {
    return { isActive: false, suppressFrontendDisconnect: true }
  }
  if (args.isLivePath) {
    return {
      isActive: true,
      // commitAcked is ignored for suppress — full detached lifetime.
      suppressFrontendDisconnect: true,
    }
  }
  return {
    isActive: args.commitAcked,
    suppressFrontendDisconnect: true,
  }
}

function normalizePhase(phase: string): string {
  return phase
    .replace(/([a-z0-9])([A-Z])/g, "$1_$2")
    .toLowerCase()
    .replace(/-/g, "_")
}

/** Poll / event terminal: enable connect when handoff completed. */
export function isHandoffCompletePhase(
  phase: string | null | undefined
): boolean {
  if (!phase) return false
  return normalizePhase(phase) === "handoff_complete"
}

export function isAbortedPhase(phase: string | null | undefined): boolean {
  if (!phase) return false
  return normalizePhase(phase) === "aborted"
}

/**
 * After claim+rebind (live) or metadata-only (cold), ready payload shape.
 */
export function buildReadyPayload(args: {
  conversationId: number
  operationId: string
  ownershipGeneration?: number | null
  connectionId?: string | null
}): {
  conversationId: number
  operationId: string
  ownershipGeneration: number | null
  connectionId: string | null
} {
  return {
    conversationId: args.conversationId,
    operationId: args.operationId,
    ownershipGeneration: args.ownershipGeneration ?? null,
    connectionId: args.connectionId ?? null,
  }
}

/**
 * Classify discovery: true cold (no connection) vs transport/API failure.
 * Discovery errors must NOT be treated as cold — that would emit ready and
 * make main release a still-owned live session.
 */
export type DiscoveryClassification =
  | { kind: "none" }
  | { kind: "live"; connectionId: string }
  | { kind: "error"; message: string }

export function classifyDiscoveryResult(args: {
  discovered: { connection_id?: string | null } | null | undefined
  error: unknown | null
  errorMessage?: string
}): DiscoveryClassification {
  if (args.error != null) {
    return {
      kind: "error",
      message:
        args.errorMessage?.trim() ||
        "Failed to discover live connection for conversation",
    }
  }
  const id = args.discovered?.connection_id
  if (id && id.length > 0) {
    return { kind: "live", connectionId: id }
  }
  return { kind: "none" }
}

/**
 * Live rebind/claim path terminal decision. Ready is only allowed on full success.
 */
export type LiveHandoffDecision =
  | {
      kind: "success"
      connectionId: string
      ownershipGeneration: number
    }
  | {
      kind: "failed"
      message: string
      /** Rebind CAS succeeded; caller should reverse with expectedGeneration. */
      rebindSucceeded: boolean
      connectionId: string | null
      ownershipGeneration: number | null
    }

/**
 * Live claim must confirm the same connection id and rebind generation before
 * the page may emit ready. Missing/mismatched generation is a claim failure.
 */
export function claimResultMatchesRebind(args: {
  claimResult:
    | { connectionId?: string; ownershipGeneration?: number }
    | null
    | undefined
  expectedConnectionId: string
  expectedOwnershipGeneration: number
}): boolean {
  const { claimResult, expectedConnectionId, expectedOwnershipGeneration } =
    args
  if (!claimResult?.connectionId) return false
  if (claimResult.connectionId !== expectedConnectionId) return false
  if (
    claimResult.ownershipGeneration == null ||
    !Number.isFinite(claimResult.ownershipGeneration)
  ) {
    return false
  }
  return claimResult.ownershipGeneration === expectedOwnershipGeneration
}

export function decideLiveHandoffResult(args: {
  connectionId: string
  rebindError: unknown | null
  rebindErrorMessage?: string
  ownershipGeneration: number | null | undefined
  claimError: unknown | null
  claimErrorMessage?: string
}): LiveHandoffDecision {
  if (args.rebindError != null) {
    return {
      kind: "failed",
      message:
        args.rebindErrorMessage?.trim() ||
        "Failed to rebind live connection ownership",
      rebindSucceeded: false,
      connectionId: args.connectionId,
      ownershipGeneration: null,
    }
  }
  const gen = args.ownershipGeneration
  if (gen == null || !Number.isFinite(gen)) {
    return {
      kind: "failed",
      message: "Rebind succeeded without ownership generation",
      rebindSucceeded: true,
      connectionId: args.connectionId,
      ownershipGeneration: typeof gen === "number" ? gen : null,
    }
  }
  if (args.claimError != null) {
    return {
      kind: "failed",
      message:
        args.claimErrorMessage?.trim() ||
        "Failed to claim live connection ownership",
      rebindSucceeded: true,
      connectionId: args.connectionId,
      ownershipGeneration: gen,
    }
  }
  return {
    kind: "success",
    connectionId: args.connectionId,
    ownershipGeneration: gen,
  }
}

/** Reverse rebind only when forward rebind CAS already moved ownership. */
export function shouldReverseRebindAfterLiveFailure(args: {
  rebindSucceeded: boolean
  ownershipGeneration: number | null | undefined
}): boolean {
  return (
    args.rebindSucceeded === true &&
    args.ownershipGeneration != null &&
    Number.isFinite(args.ownershipGeneration)
  )
}

/**
 * Mount the full session surface (including MessageListView overlays) only
 * when activation is allowed — claimed live path, or cold after commit-ack.
 */
export function shouldMountDetachedSurface(args: {
  valid: boolean
  hasError: boolean
  bootstrapReady: boolean
  readyEmitted: boolean
  isActive: boolean
}): boolean {
  return (
    args.valid &&
    !args.hasError &&
    args.bootstrapReady &&
    args.readyEmitted &&
    args.isActive
  )
}

/**
 * React 19 parent-before-child unmount order: never clear suppress in a parent
 * unmount effect. Suppress lasts the full detached window lifetime and dies
 * with the JS context so descendant lifecycle teardown cannot bare-acpDisconnect.
 */
export function shouldClearSuppressOnDetachedUnmount(): boolean {
  return false
}

/**
 * Commit-ack must not clear suppress either. Detached owners keep
 * viewer-style disconnect (no acpDisconnect) until the window process exits.
 */
export function shouldClearSuppressOnDetachedCommitAck(): boolean {
  return false
}
