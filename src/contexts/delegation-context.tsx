"use client"

/**
 * DelegationContext — tracks live parent ↔ child delegation bindings
 * indexed by `parent_tool_use_id`.
 *
 * The parent's `delegate_to_agent` ToolCallBlock needs to render the child
 * sub-session inline. Both wire events (`delegation_started` /
 * `delegation_completed`) are emitted on the *parent*'s connection stream by
 * the broker, so this context subscribes via the provider's `useAcpEvent`
 * fanout — which is fed by the Tauri firehose AND the per-connection attach
 * streams, so it behaves identically in desktop and web/server runtimes. It
 * filters the two delegation variants and exposes a tool-use-id-keyed lookup
 * so ToolCallBlock can resolve the binding by the field it already has in hand.
 *
 * Map transitions are pure (`applyDelegationEnvelope`). Attach / detach grace
 * timers are the only side effects here — detach runs only when a completion
 * is accepted (matching `task_id` or synthesized when no binding exists).
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
  useRef,
  useState,
} from "react"

import type { EventEnvelope } from "@/lib/types"
import {
  applyDelegationEnvelope,
  type DelegationBinding,
  type DelegationStatus,
} from "@/lib/delegation-binding-reduce"
import { useAcpActions, useAcpEvent } from "@/contexts/acp-connections-context"

export type { DelegationBinding, DelegationStatus }

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

/** Grace period after `delegation_completed` before tearing down the
 *  synthetic child ConnectionState. Long enough for the parent UI to
 *  finish rendering the child's final assistant text from live state
 *  before falling through to the DB-persisted view. */
const CHILD_DETACH_GRACE_MS = 2_000

export function DelegationProvider({ children }: { children: ReactNode }) {
  const { attachDelegationChild, detachDelegationChild } = useAcpActions()
  const [byToolUseId, setByToolUseId] = useState<
    Map<string, DelegationBinding>
  >(() => new Map())

  // Mirror of map state so sequential envelopes in one tick apply in order
  // without waiting for React to commit (and so detach decisions use the
  // same pure transition as the map update).
  const mapRef = useRef(byToolUseId)
  useEffect(() => {
    mapRef.current = byToolUseId
  }, [byToolUseId])

  // Stable refs so the event-subscription effect doesn't tear down on
  // every action identity change (the actions object is memoized but
  // its members are stable callbacks; still, defensive ref-pinning
  // keeps the subscription stable across React's StrictMode double-effect).
  const attachRef = useRef(attachDelegationChild)
  const detachRef = useRef(detachDelegationChild)
  useEffect(() => {
    attachRef.current = attachDelegationChild
  }, [attachDelegationChild])
  useEffect(() => {
    detachRef.current = detachDelegationChild
  }, [detachDelegationChild])

  // Pending detach timers — one per parent_tool_use_id. Started on
  // accepted `delegation_completed`, cleared if a fresh `delegation_started`
  // arrives for the same parent_tool_use_id before the timer fires.
  const detachTimersRef = useRef(
    new Map<string, ReturnType<typeof setTimeout>>()
  )

  const cancelDetachTimer = useCallback((parentToolUseId: string) => {
    const timers = detachTimersRef.current
    const t = timers.get(parentToolUseId)
    if (t) {
      clearTimeout(t)
      timers.delete(parentToolUseId)
    }
  }, [])

  const handleEnvelope = useCallback(
    (envelope: EventEnvelope) => {
      const { next, acceptedCompletionToolUseId } = applyDelegationEnvelope(
        mapRef.current,
        envelope
      )
      mapRef.current = next
      setByToolUseId(next)

      if (envelope.type === "delegation_started") {
        // Cancel any pending detach for this parent_tool_use_id —
        // delegation_started can be replayed after a partial flow
        // (e.g. reconnect), and an in-flight detach would tear the
        // child state down right as it returns.
        cancelDetachTimer(envelope.parent_tool_use_id)
        // Pull the child connection into the reducer so its
        // streaming text / tool calls / pendingPermission reach
        // the parent's DelegatedSubThread inline.
        attachRef.current({
          connectionId: envelope.child_connection_id,
          parentConnectionId: envelope.parent_connection_id,
          parentToolUseId: envelope.parent_tool_use_id,
          agentType: envelope.agent_type,
        })
        return
      }

      // Detach ONLY when completion was accepted (match or synthesize).
      // Mismatched task_id → acceptedCompletionToolUseId is null → no timer.
      if (
        acceptedCompletionToolUseId !== null &&
        envelope.type === "delegation_completed"
      ) {
        const parentToolUseId = acceptedCompletionToolUseId
        const childConnectionId = envelope.child_connection_id
        cancelDetachTimer(parentToolUseId)
        const timer = setTimeout(() => {
          detachTimersRef.current.delete(parentToolUseId)
          detachRef.current(childConnectionId)
        }, CHILD_DETACH_GRACE_MS)
        detachTimersRef.current.set(parentToolUseId, timer)
      }
    },
    [cancelDetachTimer]
  )

  // Single subscription via the provider's fanout. `useAcpEvent` fires for
  // every mapped envelope on both the Tauri firehose and the per-connection
  // attach streams, so the parent-stream delegation events reach us in both
  // desktop and web/server runtimes; non-delegation types are ignored above.
  useAcpEvent(handleEnvelope)

  // Clear any pending detach timers on unmount. The synthetic children are
  // also cleaned up by the connections context's own teardown.
  useEffect(() => {
    const timers = detachTimersRef.current
    return () => {
      for (const t of timers.values()) clearTimeout(t)
      timers.clear()
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
