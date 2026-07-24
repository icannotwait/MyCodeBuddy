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
  // Synced via effect (project convention for react-hooks/refs). Event handlers
  // run after paint, so parent_id for conversation://changed is always current.
  const accessRef = useRef(access)
  useEffect(() => {
    accessRef.current = access
  }, [access])
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
    let disposed = false
    let dispose: (() => void) | undefined
    let retryTimer: ReturnType<typeof setTimeout> | null = null
    let retryIndex = 0
    let inFlight: Promise<void> | null = null
    let rerun = false

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
          // Keep loading (fail-closed) if a deferred single-flight follow-up
          // is queued so we never flash interactive between coalesced runs.
          setSnapshot({ scope, access: next, loading: rerun })
        })
        .catch(() => {
          if (disposed) return
          const keepLoading = rerun
          setSnapshot((current) => ({
            scope,
            loading: keepLoading,
            access: failClosedAccess(
              current.scope === scope ? current.access.parent_id : null
            ),
          }))
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

    const scopeRefresh = () => run(true)
    requestRefreshRef.current = scopeRefresh

    // Subscribe first, then load. Starting the access read before the event
    // subscription is ready can miss parent/child lock events that arrive in
    // the gap; the post-subscribe run is both the initial load and catch-up.
    void subscribe<ConversationChange>(CONVERSATION_CHANGED_EVENT, (change) => {
      const id = changedId(change)
      const current = accessRef.current
      if (id === conversationId || id === current.parent_id) void run(true)
    }).then((off) => {
      if (disposed) {
        off()
        return
      }
      dispose = off
      if (enabled && conversationId != null) void run(true)
    })
    const offReconnect = onTransportReconnect(() => void run(true))
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
