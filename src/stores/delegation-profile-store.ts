"use client"

import { useEffect } from "react"
import { create } from "zustand"
import { getDelegationProfileCatalog } from "@/lib/api"
import { toErrorMessage } from "@/lib/app-error"
import { onTransportReconnect, subscribe } from "@/lib/platform"
import type { UnsubscribeFn } from "@/lib/transport/types"
import {
  DELEGATION_PROFILE_CATALOG_CHANGED_EVENT,
  type DelegationProfileCatalog,
} from "@/lib/types"
import { registerBackendScopedStoreReset } from "@/stores/backend-scoped-store-reset"

export type { DelegationProfileCatalog }

interface DelegationProfileState {
  ready: boolean
  error: string | null
  catalog: DelegationProfileCatalog | null
  /** Apply only when `incoming.revision > current.revision` (or no current). */
  applyCatalog: (incoming: DelegationProfileCatalog) => void
  /**
   * Idempotent: installs one event subscription + focus/reconnect callbacks
   * and owns one initial in-flight getter. Repeated calls are no-ops.
   */
  initialize: () => Promise<void>
  /**
   * Always fetches a catalog. Coalesces only a currently in-flight refresh;
   * retains the last good catalog on failure. Focus/reconnect invoke this.
   */
  refresh: () => Promise<void>
}

let initialized = false
let bootstrapInFlight: Promise<void> | null = null
let eventUnsub: UnsubscribeFn | null = null
let eventDisposed = false
let reconnectUnsub: (() => void) | null = null
let focusListener: (() => void) | null = null
let refreshInFlight: Promise<void> | null = null
/** Bumped on dispose so in-flight initialize/bootstrap cannot re-apply. */
let disposeGeneration = 0

// Ref-counted bootstrap so AppWorkspaceProvider + future consumers share one
// subscription; final release disposes listeners.
let bootstrapRefCount = 0

export const useDelegationProfileStore = create<DelegationProfileState>(
  (set, get) => ({
    ready: false,
    error: null,
    catalog: null,

    applyCatalog: (incoming) => {
      const current = get().catalog
      if (current && incoming.revision <= current.revision) return
      set({ catalog: incoming, ready: true, error: null })
    },

    initialize: async () => {
      if (initialized) return
      initialized = true
      const generation = disposeGeneration

      // Install listeners before the bootstrap fetch so concurrent catalog
      // events during cold start are not dropped (revision gate is order-safe).
      eventDisposed = false
      void subscribe<DelegationProfileCatalog>(
        DELEGATION_PROFILE_CATALOG_CHANGED_EVENT,
        (catalog) => {
          get().applyCatalog(catalog)
        }
      )
        .then((dispose) => {
          if (eventDisposed) {
            dispose()
            return
          }
          eventUnsub = dispose
        })
        .catch(() => {
          // Transport doesn't support subscribe — refresh-only path.
        })

      focusListener = () => {
        void get().refresh()
      }
      window.addEventListener("focus", focusListener)

      reconnectUnsub =
        onTransportReconnect(() => {
          void get().refresh()
        }) ?? null

      bootstrapInFlight = (async () => {
        try {
          const catalog = await getDelegationProfileCatalog()
          if (disposeGeneration !== generation) return
          get().applyCatalog(catalog)
        } catch (err: unknown) {
          if (disposeGeneration !== generation) return
          set({ ready: true, error: toErrorMessage(err) })
        } finally {
          bootstrapInFlight = null
        }
      })()
      await bootstrapInFlight
    },

    refresh: async () => {
      if (refreshInFlight) {
        await refreshInFlight
        return
      }
      refreshInFlight = (async () => {
        try {
          const response = await getDelegationProfileCatalog()
          get().applyCatalog(response)
          // Clear a transient error even when the revision gate declines to
          // rewrite an equal-revision catalog.
          set({ ready: true, error: null })
        } catch (err: unknown) {
          set({ ready: true, error: toErrorMessage(err) })
        }
      })()
      try {
        await refreshInFlight
      } finally {
        refreshInFlight = null
      }
    },
  })
)

function disposeSharedSubscription(): void {
  disposeGeneration += 1
  eventDisposed = true
  if (eventUnsub) {
    try {
      eventUnsub()
    } catch {
      // ignore
    }
    eventUnsub = null
  }
  if (reconnectUnsub) {
    try {
      reconnectUnsub()
    } catch {
      // ignore
    }
    reconnectUnsub = null
  }
  if (focusListener) {
    try {
      window.removeEventListener("focus", focusListener)
    } catch {
      // ignore
    }
    focusListener = null
  }
  initialized = false
  bootstrapInFlight = null
  refreshInFlight = null
}

/**
 * Mount-time bootstrap: ref-counted so multiple consumers create one
 * subscription and the final release disposes it.
 */
export function useDelegationProfileBootstrap(): void {
  useEffect(() => {
    bootstrapRefCount += 1
    void useDelegationProfileStore.getState().initialize()
    return () => {
      bootstrapRefCount -= 1
      if (bootstrapRefCount === 0) {
        disposeSharedSubscription()
      }
    }
  }, [])
}

export function resetDelegationProfileStore(): void {
  disposeSharedSubscription()
  bootstrapRefCount = 0
  useDelegationProfileStore.setState({
    ready: false,
    error: null,
    catalog: null,
  })
}

registerBackendScopedStoreReset(resetDelegationProfileStore)
