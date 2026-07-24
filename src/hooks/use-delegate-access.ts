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
  // Scope mismatch is synchronously fail-closed during render. Waiting for an
  // effect here would leave one frame where child B inherits child A's unlock.
  const access = !enabled
    ? NON_DELEGATE_ACCESS
    : snapshot.scope === scope
      ? snapshot.access
      : UNKNOWN_DELEGATE_ACCESS
  const loading = enabled && (snapshot.scope !== scope || snapshot.loading)
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
          setSnapshot({ scope, access: next, loading: false })
        })
        .catch(() => {
          if (disposed) return
          setSnapshot((current) => ({
            scope,
            loading: false,
            access: {
              ...UNKNOWN_DELEGATE_ACCESS,
              parent_id:
                current.scope === scope ? current.access.parent_id : null,
            },
          }))
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
          setSnapshot((current) =>
            current.scope === scope ? { ...current, loading: false } : current
          )
          if (rerun) {
            rerun = false
            cancelRetry()
            queueMicrotask(() => void run(true))
          }
        })
      inFlight = request
      return request
    }

    const scopeRefresh = () => run(true)
    requestRefreshRef.current = scopeRefresh
    if (enabled && conversationId != null) void run(true)
    void subscribe<ConversationChange>(CONVERSATION_CHANGED_EVENT, (change) => {
      const id = changedId(change)
      const current = accessRef.current
      if (id === conversationId || id === current.parent_id) void run(true)
    }).then((off) => {
      if (disposed) off()
      else dispose = off
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
