"use client"

/**
 * DelegationContext — tracks live parent ↔ child delegation bindings
 * indexed by `parent_tool_use_id`.
 *
 * The parent's `delegate_to_agent` row needs to show the child
 * sub-session inline. Both wire events (`delegation_started` /
 * `delegation_completed`) are emitted on the *parent*'s connection stream by
 * the broker, so this context subscribes via the provider's `useAcpEvent`
 * fanout — which is fed by the Tauri firehose AND the per-connection attach
 * streams, so it behaves identically in desktop and web/server runtimes. It
 * filters the two delegation variants and exposes a tool-use-id-keyed lookup
 * so the row can resolve the binding by the field it already has in hand.
 *
 * Map transitions are pure (`applyDelegationEnvelope`). Attach / detach grace
 * timers are the only side effects here — detach runs only when a completion
 * is accepted (matching `task_id` or synthesized when no binding exists).
 *
 * State stays in-memory; persistence across reloads relies on the
 * parent_tool_use_id stored on the child's DB row (Phase 7).
 *
 * On the child's blocking prompts: attaching the child (below) is what makes
 * them visible at all. The permission store stays per-connection — a child's
 * `permission_request` is never re-keyed onto the parent's row — but
 * because the child IS a connection in the store, `useDelegationCardModel`
 * reads its pending prompts directly and badges the delegation row
 * "waiting", and both `SubAgentSessionDialog` and the `get_delegation_status`
 * row (`useDelegationTaskLive`, found by task id) answer through the child's
 * own connection id. Surfaces OUTSIDE this transcript (the pet badge/panel,
 * the work-task row, the parent LLM's view of `get_delegation_status`) are
 * handled in the backend, not here — see `pet_list_active_sessions_core` and
 * the broker's `blocked_on` (#447).
 */

import {
  type ReactNode,
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react"

import type { EventEnvelope } from "@/lib/types"
import {
  applyDelegationEnvelope,
  type DelegationBinding as ReducerDelegationBinding,
  type DelegationStatus,
} from "@/lib/delegation-binding-reduce"
import { useWorkflowGraphStore } from "@/lib/workflow-graph-store"
import {
  type AcpEventDelivery,
  useAcpActions,
  useAcpEvent,
} from "@/contexts/acp-connections-context"

/** Fork binding plus the assistant the child runs as. The reducer owns
 *  lifecycle; assistant identity is stamped here from the same envelopes. */
export interface DelegationBinding extends ReducerDelegationBinding {
  assistantId: number | null
  assistantName: string | null
}

export type { DelegationStatus }

export interface DelegationContextValue {
  findByParentToolUseId(id: string): DelegationBinding | undefined
  findByChildConversationId(id: number): DelegationBinding | undefined
  /**
   * Resolve a binding by the broker-minted task id rather than the parent's
   * tool_use_id. `resume_delegation` needs this: it re-binds the child to the
   * ORIGINAL `delegate_to_agent` call's id, so the resume card's own
   * tool_call_id matches no binding — the task id is the only handle it holds.
   */
  findByTaskId(taskId: string): DelegationBinding | undefined
  /**
   * Every binding a parent connection's delegations left here, finished ones
   * included, in the order they started. The composer's running list takes
   * WHICH delegations still run from the parent's own `delegatedChildren`
   * (the backend's word, recovered by a snapshot) and only their identity from
   * here: a binding stays `running` forever if its completion event was
   * missed, and an async delegation is not tied to any one reply.
   */
  bindingsForParent(parentConnectionId: string): DelegationBinding[]
}

function bindingStartedAtMs(binding: DelegationBinding): number {
  const value = Date.parse(binding.startedAt)
  return Number.isFinite(value) ? value : Number.NEGATIVE_INFINITY
}

/** Running wins over terminal; otherwise newest start and stable id win. */
export function selectPreferredDelegationBinding(
  left: DelegationBinding | undefined,
  right: DelegationBinding | undefined
): DelegationBinding | undefined {
  if (!left) return right
  if (!right) return left
  const leftRunning = left.status === "running"
  const rightRunning = right.status === "running"
  if (leftRunning !== rightRunning) return rightRunning ? right : left

  const leftStartedAt = bindingStartedAtMs(left)
  const rightStartedAt = bindingStartedAtMs(right)
  if (leftStartedAt !== rightStartedAt) {
    return rightStartedAt > leftStartedAt ? right : left
  }
  return right.parentToolUseId > left.parentToolUseId ? right : left
}

const DelegationContext = createContext<DelegationContextValue | null>(null)

export function useDelegation(): DelegationContextValue {
  const ctx = useContext(DelegationContext)
  if (!ctx) {
    throw new Error("useDelegation must be used within DelegationProvider")
  }
  return ctx
}

/**
 * Fixed lookups in place of the live provider — for surfaces that render chat
 * cards outside a workspace (the activity gallery page, component tests).
 */
export function StaticDelegationProvider({
  value,
  children,
}: {
  value: DelegationContextValue
  children: ReactNode
}) {
  return (
    <DelegationContext.Provider value={value}>
      {children}
    </DelegationContext.Provider>
  )
}

/** The lookups, or `null` outside `DelegationProvider` — for a card that only
 *  enriches itself from a live binding when there is one. */
export function useOptionalDelegation(): DelegationContextValue | null {
  return useContext(DelegationContext)
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
    (envelope: EventEnvelope, delivery?: AcpEventDelivery) => {
      const applied = applyDelegationEnvelope(
        mapRef.current as Map<string, ReducerDelegationBinding>,
        envelope
      )
      const next = new Map(applied.next) as Map<string, DelegationBinding>
      const { acceptedCompletionToolUseId } = applied

      const stampAssistant = (toolUseId: string, clearWhenAbsent: boolean) => {
        if (
          envelope.type !== "delegation_started" &&
          envelope.type !== "delegation_completed"
        ) {
          return
        }
        const current = next.get(toolUseId)
        if (!current) return
        const assistantId = clearWhenAbsent
          ? (envelope.assistant_id ?? null)
          : (envelope.assistant_id ?? current.assistantId ?? null)
        const assistantName = clearWhenAbsent
          ? (envelope.assistant_name ?? null)
          : (envelope.assistant_name ?? current.assistantName ?? null)
        next.set(toolUseId, { ...current, assistantId, assistantName })
      }

      if (envelope.type === "delegation_started") {
        stampAssistant(envelope.parent_tool_use_id, true)
      } else if (
        envelope.type === "delegation_completed" &&
        acceptedCompletionToolUseId !== null
      ) {
        stampAssistant(acceptedCompletionToolUseId, false)
      }

      mapRef.current = next
      setByToolUseId(next)

      if (envelope.type === "delegation_runtime_stats_changed") {
        useWorkflowGraphStore
          .getState()
          .applyRuntimeStats(envelope.task_id, envelope.runtime_stats)
      }

      if (envelope.type === "delegation_started") {
        // Cancel any pending detach for this parent_tool_use_id —
        // delegation_started can be replayed after a partial flow
        // (e.g. reconnect), and an in-flight detach would tear the
        // child state down right as it returns.
        cancelDetachTimer(envelope.parent_tool_use_id)
        // Pull the child connection into the reducer so its
        // streaming text / tool calls / pendingPermission reach
        // the parent's delegation row. A binding re-seeded from
        // the parent's snapshot names a child that is already mid-turn —
        // possibly parked on a permission it raised before this client
        // attached — so that one is hydrated from the child's own snapshot.
        attachRef.current({
          connectionId: envelope.child_connection_id,
          parentConnectionId: envelope.parent_connection_id,
          parentToolUseId: envelope.parent_tool_use_id,
          agentType: envelope.agent_type,
          hydrate: delivery?.seededFromSnapshot === true,
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
      let selected: DelegationBinding | undefined
      for (const b of byToolUseId.values()) {
        if (b.childConversationId !== id) continue
        selected = selectPreferredDelegationBinding(selected, b)
      }
      return selected
    },
    [byToolUseId]
  )

  const findByTaskId = useCallback(
    (taskId: string): DelegationBinding | undefined => {
      if (!taskId) return undefined
      for (const b of byToolUseId.values()) {
        if (b.taskId === taskId) return b
      }
      return undefined
    },
    [byToolUseId]
  )

  const bindingsForParent = useCallback(
    (parentConnectionId: string): DelegationBinding[] => {
      const out: DelegationBinding[] = []
      if (!parentConnectionId) return out
      // Map order is insertion order: a binding is inserted when its
      // delegation starts (a re-start replaces it in place).
      for (const b of byToolUseId.values()) {
        if (b.parentConnectionId === parentConnectionId) out.push(b)
      }
      return out
    },
    [byToolUseId]
  )

  const value = useMemo(
    () => ({
      findByParentToolUseId,
      findByChildConversationId,
      findByTaskId,
      bindingsForParent,
    }),
    [
      findByParentToolUseId,
      findByChildConversationId,
      findByTaskId,
      bindingsForParent,
    ]
  )

  return (
    <DelegationContext.Provider value={value}>
      {children}
    </DelegationContext.Provider>
  )
}
