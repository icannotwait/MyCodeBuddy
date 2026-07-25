"use client"

import { useCallback, useEffect, useRef, useState } from "react"
import { getDelegateAccess } from "@/lib/api"
import {
  NON_DELEGATE_ACCESS,
  UNKNOWN_DELEGATE_ACCESS,
} from "@/lib/delegate-access"
import { onTransportReconnect, subscribe } from "@/lib/platform"
import {
  CONVERSATION_CHANGED_EVENT,
  type ConversationChange,
  type DelegateAccessState,
} from "@/lib/types"

export interface UseDelegateAccessArgs {
  conversationId: number | null
  enabled: boolean
}

const ACCESS_LOOKUP_RETRY_DELAYS_MS = [300, 700, 1500, 2500] as const

function changedId(change: ConversationChange): number {
  return change.kind === "upsert"
    ? change.summary.id
    : change.kind === "deleted"
      ? change.id
      : change.patch.id
}

function failClosedAccess(parentId: number | null = null): DelegateAccessState {
  return {
    ...UNKNOWN_DELEGATE_ACCESS,
    parent_id: parentId,
  }
}

export function useDelegateAccess({
  conversationId,
  enabled,
}: UseDelegateAccessArgs) {
  const scope =
    enabled && conversationId != null
      ? `delegate:${conversationId}`
      : "disabled"
  const [snapshot, setSnapshot] = useState<{
    scope: string
    access: DelegateAccessState
    loading: boolean
  }>(() => ({
    scope,
    access: enabled ? UNKNOWN_DELEGATE_ACCESS : NON_DELEGATE_ACCESS,
    loading: enabled,
  }))
  // Scope mismatch and in-flight revalidation are synchronously fail-closed
  // during render. Plan: loading/error states are always viewer_only so a
  // refresh after a lock-relevant event never keeps presenting interactive.
  const loading = enabled && (snapshot.scope !== scope || snapshot.loading)
  const access = !enabled
    ? NON_DELEGATE_ACCESS
    : loading
      ? failClosedAccess(
          snapshot.scope === scope ? snapshot.access.parent_id : null
        )
      : snapshot.access
  // parent_id for conversation://changed matching must not lag a successful
  // fetch (a passive effect would miss parent events that arrive before paint).
  // Updated synchronously when run() applies an access snapshot.
  const parentIdRef = useRef<number | null>(null)
  const requestRefreshRef = useRef<() => Promise<void>>(async () => undefined)
  const refresh = useCallback(
    (): Promise<void> => requestRefreshRef.current(),
    []
  )

  useEffect(() => {
    // Intentional scope reset: render already fail-closes on scope mismatch, but
    // we still commit the new scope so in-flight fetch handlers write into a
    // consistent snapshot and loading flags clear correctly for this child.
    // eslint-disable-next-line react-hooks/set-state-in-effect -- scope remount
    setSnapshot({
      scope,
      access: enabled ? UNKNOWN_DELEGATE_ACCESS : NON_DELEGATE_ACCESS,
      loading: enabled,
    })
    parentIdRef.current = null
    let disposed = false
    let dispose: (() => void) | undefined
    let retryTimer: ReturnType<typeof setTimeout> | null = null
    let retryIndex = 0
    let inFlight: Promise<void> | null = null
    let rerun = false
    // Gate every refresh source until conversation://changed is subscribed so
    // we never race a fetch ahead of event delivery. Early refresh/reconnect
    // requests queue and flush once ready.
    let subscriptionReady = false
    let queuedRefresh = false

    const cancelRetry = () => {
      if (retryTimer) clearTimeout(retryTimer)
      retryTimer = null
    }
    const run = (resetBackoff: boolean): Promise<void> => {
      if (disposed || !enabled || conversationId == null) {
        return Promise.resolve()
      }
      if (resetBackoff) {
        retryIndex = 0
        cancelRetry()
      }
      if (inFlight) {
        rerun = true
        return inFlight
      }
      setSnapshot((current) =>
        current.scope === scope ? { ...current, loading: true } : current
      )
      const request = getDelegateAccess(conversationId)
        .then((next) => {
          if (disposed) return
          retryIndex = 0
          cancelRetry()
          // Keep parent matching current immediately — do not wait for a
          // passive effect after setSnapshot.
          parentIdRef.current = next.parent_id
          // Keep loading (fail-closed) if a deferred single-flight follow-up
          // is queued so we never flash interactive between coalesced runs.
          setSnapshot({ scope, access: next, loading: rerun })
        })
        .catch(() => {
          if (disposed) return
          const keepLoading = rerun
          setSnapshot((current) => {
            const parentId =
              current.scope === scope ? current.access.parent_id : null
            // Preserve known parent for event matching across lookup failures.
            if (current.scope === scope) {
              parentIdRef.current = parentId
            }
            return {
              scope,
              loading: keepLoading,
              access: failClosedAccess(parentId),
            }
          })
          if (keepLoading) return
          const delay = ACCESS_LOOKUP_RETRY_DELAYS_MS[retryIndex]
          if (delay !== undefined) {
            retryIndex += 1
            retryTimer = setTimeout(() => {
              retryTimer = null
              void run(false)
            }, delay)
          }
        })
        .finally(() => {
          if (inFlight === request) inFlight = null
          if (disposed) return
          if (rerun) {
            rerun = false
            cancelRetry()
            setSnapshot((current) =>
              current.scope === scope ? { ...current, loading: true } : current
            )
            queueMicrotask(() => void run(true))
            return
          }
          setSnapshot((current) =>
            current.scope === scope ? { ...current, loading: false } : current
          )
        })
      inFlight = request
      return request
    }

    // All external refresh entry points go through requestRun so a pending
    // subscription never starts a fetch (manual refresh, reconnect, events).
    const requestRun = (resetBackoff: boolean): Promise<void> => {
      if (disposed || !enabled || conversationId == null) {
        return Promise.resolve()
      }
      if (!subscriptionReady) {
        queuedRefresh = true
        return Promise.resolve()
      }
      return run(resetBackoff)
    }

    const scopeRefresh = () => requestRun(true)
    requestRefreshRef.current = scopeRefresh

    void subscribe<ConversationChange>(CONVERSATION_CHANGED_EVENT, (change) => {
      const id = changedId(change)
      if (id === conversationId || id === parentIdRef.current) {
        void requestRun(true)
      }
    }).then((off) => {
      if (disposed) {
        off()
        return
      }
      dispose = off
      subscriptionReady = true
      // Initial load (and any refresh/reconnect queued while subscribe was
      // pending). One run covers both; single-flight coalesces further demand.
      if (enabled && conversationId != null) {
        queuedRefresh = false
        void run(true)
      } else if (queuedRefresh) {
        queuedRefresh = false
      }
    })
    const offReconnect = onTransportReconnect(() => {
      void requestRun(true)
    })
    return () => {
      disposed = true
      cancelRetry()
      if (requestRefreshRef.current === scopeRefresh) {
        requestRefreshRef.current = async () => undefined
      }
      dispose?.()
      offReconnect?.()
    }
  }, [conversationId, enabled, scope])

  return { access, loading, refresh }
}
