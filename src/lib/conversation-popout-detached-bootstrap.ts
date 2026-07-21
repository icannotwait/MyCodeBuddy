/**
 * Pure bootstrap gates for the detached conversation window.
 * Keeps claim-before-activate / cold-until-commit-ack rules testable
 * without mounting the full React page.
 */

import type { AgentType } from "@/lib/types"

export const CONVERSATION_WINDOW_READY_EVENT = "conversation-window://ready"
export const CONVERSATION_WINDOW_COMMIT_ACK_EVENT =
  "conversation-window://commit-ack"

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
 * - Live: isActive only after claim succeeds; suppress disconnect until commit-ack
 * - Cold: isActive false until commit-ack; suppress until commit-ack
 * - Before bootstrap ready: never auto-connect
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
      suppressFrontendDisconnect: !args.commitAcked,
    }
  }
  return {
    isActive: args.commitAcked,
    suppressFrontendDisconnect: !args.commitAcked,
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
