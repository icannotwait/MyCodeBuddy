"use client"

/**
 * DelegationContext — tracks live parent ↔ child delegation bindings
 * indexed by `parent_tool_use_id`.
 *
 * The parent's `delegate_to_agent` ToolCallBlock needs to render the child
 * sub-session inline, but the wire events (`delegation_started` /
 * `delegation_completed`) arrive on the *child*'s connection stream — there
 * is no per-connection-keyed subscription that gives the parent UI access to
 * them. This context owns a single global subscription to `acp://event`,
 * filters the two delegation variants, and exposes a tool-use-id-keyed
 * lookup so ToolCallBlock can resolve the binding by the field it already
 * has in hand.
 *
 * Scope intentionally minimal for Phase 8:
 *   * State stays in-memory; persistence across reloads relies on the
 *     parent_tool_use_id stored on the child's DB row (Phase 7).
 *   * Inline permission routing (child's `permission_request` surfaced on
 *     parent's ToolCallBlock) is deferred — the existing permission store
 *     is per-connection and would require a broader reducer change.
 */

import {
  type ReactNode,
  createContext,
  useCallback,
  useContext,
  useEffect,
  useState,
} from "react"

import type { AgentType, EventEnvelope } from "@/lib/types"
import { subscribe } from "@/lib/platform"

export type DelegationStatus = "running" | "ok" | "err"

export interface DelegationBinding {
  parentConnectionId: string
  parentToolUseId: string
  childConnectionId: string
  childConversationId: number
  agentType: AgentType
  status: DelegationStatus
  errorCode?: string
  durationMs?: number
}

interface DelegationContextValue {
  findByParentToolUseId(id: string): DelegationBinding | undefined
  findByChildConversationId(id: number): DelegationBinding | undefined
}

const DelegationContext = createContext<DelegationContextValue | null>(null)

export function useDelegation(): DelegationContextValue {
  const ctx = useContext(DelegationContext)
  if (!ctx) {
    throw new Error("useDelegation must be used within DelegationProvider")
  }
  return ctx
}

export function DelegationProvider({ children }: { children: ReactNode }) {
  const [byToolUseId, setByToolUseId] = useState<
    Map<string, DelegationBinding>
  >(() => new Map())

  useEffect(() => {
    let unsubscribed = false
    let unsubscribe: (() => void) | null = null

    void (async () => {
      const unsub = await subscribe<EventEnvelope>(
        "acp://event",
        (envelope) => {
          if (envelope.type === "delegation_started") {
            const next: DelegationBinding = {
              parentConnectionId: envelope.parent_connection_id,
              parentToolUseId: envelope.parent_tool_use_id,
              childConnectionId: envelope.child_connection_id,
              childConversationId: envelope.child_conversation_id,
              agentType: envelope.agent_type,
              status: "running",
            }
            setByToolUseId((prev) => {
              const m = new Map(prev)
              m.set(envelope.parent_tool_use_id, next)
              return m
            })
            return
          }
          if (envelope.type === "delegation_completed") {
            setByToolUseId((prev) => {
              const existing = prev.get(envelope.parent_tool_use_id)
              // If we missed the start event (e.g. context mounted mid-flight),
              // synthesize a minimal binding so the parent UI still shows the
              // result. Fields not in the completion payload stay defaulted.
              const base: DelegationBinding = existing ?? {
                parentConnectionId: envelope.parent_connection_id,
                parentToolUseId: envelope.parent_tool_use_id,
                childConnectionId: envelope.child_connection_id,
                childConversationId: envelope.child_conversation_id,
                agentType: "claude_code",
                status: "running",
              }
              const updated: DelegationBinding =
                envelope.result.kind === "ok"
                  ? {
                      ...base,
                      status: "ok",
                      durationMs: envelope.result.duration_ms,
                    }
                  : {
                      ...base,
                      status: "err",
                      errorCode: envelope.result.error_code,
                    }
              const m = new Map(prev)
              m.set(envelope.parent_tool_use_id, updated)
              return m
            })
          }
        }
      )

      if (unsubscribed) {
        unsub()
      } else {
        unsubscribe = unsub
      }
    })()

    return () => {
      unsubscribed = true
      unsubscribe?.()
    }
  }, [])

  const findByParentToolUseId = useCallback(
    (id: string): DelegationBinding | undefined => byToolUseId.get(id),
    [byToolUseId]
  )

  const findByChildConversationId = useCallback(
    (id: number): DelegationBinding | undefined => {
      for (const b of byToolUseId.values()) {
        if (b.childConversationId === id) return b
      }
      return undefined
    },
    [byToolUseId]
  )

  return (
    <DelegationContext.Provider
      value={{ findByParentToolUseId, findByChildConversationId }}
    >
      {children}
    </DelegationContext.Provider>
  )
}
