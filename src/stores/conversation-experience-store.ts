"use client"

import { useEffect } from "react"
import { create } from "zustand"
import {
  getConversationExperienceSettings,
  setAutoTitleApiConfig as setAutoTitleApiConfigApi,
  setDocumentTranslateAgent as setDocumentTranslateAgentApi,
  setReferenceSearchLimit as setReferenceSearchLimitApi,
  type SetAutoTitleApiConfigParams,
} from "@/lib/api"
import { onTransportReconnect, subscribe } from "@/lib/platform"
import type { UnsubscribeFn } from "@/lib/transport/types"
import {
  CONVERSATION_EXPERIENCE_SETTINGS_CHANGED_EVENT,
  type AgentType,
  type ConversationExperienceSettings,
} from "@/lib/types"
import { registerBackendScopedStoreReset } from "@/stores/backend-scoped-store-reset"

export type { ConversationExperienceSettings }

interface ConversationExperienceState {
  settings: ConversationExperienceSettings | null
  loading: boolean
  /**
   * True after a fetch finished with no usable snapshot (cold-start or retry).
   * When true and `settings` is null, the settings UI must not render a
   * writable form — empty drafts would clobber a real server config on Save.
   */
  loadError: boolean
  /** Apply only when `snapshot.revision > current.revision` (or no current). */
  applySnapshot: (snapshot: ConversationExperienceSettings) => void
  /**
   * Idempotent: installs one event subscription + reconnect callback and owns
   * one initial in-flight getter. Repeated calls are no-ops while initialized.
   */
  initialize: () => void
  /**
   * Always fetches a snapshot. Coalesces only a currently in-flight refresh;
   * retains the last good snapshot on failure. Reconnect invokes this.
   * When no snapshot exists yet, surfaces loading and clears/sets loadError.
   */
  refresh: () => Promise<void>
  setAutoTitleApiConfig: (
    params: SetAutoTitleApiConfigParams
  ) => Promise<ConversationExperienceSettings>
  setDocumentTranslateAgent: (
    agent: AgentType | null
  ) => Promise<ConversationExperienceSettings>
  setReferenceSearchLimit: (
    limit: number
  ) => Promise<ConversationExperienceSettings>
}

let initialized = false
let eventUnsub: UnsubscribeFn | null = null
let eventDisposed = false
let reconnectUnsub: (() => void) | null = null
let refreshInFlight: Promise<void> | null = null

// Ref-counted bootstrap so AppWorkspaceProvider + settings section share one
// subscription; final release disposes listeners.
let bootstrapRefCount = 0

export const useConversationExperienceStore =
  create<ConversationExperienceState>((set, get) => ({
    settings: null,
    loading: false,
    loadError: false,

    applySnapshot: (snapshot) => {
      const current = get().settings
      if (current != null && snapshot.revision <= current.revision) {
        return
      }
      set({ settings: snapshot, loadError: false })
    },

    initialize: () => {
      if (initialized) return
      initialized = true

      void (async () => {
        set({ loading: true, loadError: false })
        try {
          const snapshot = await getConversationExperienceSettings()
          get().applySnapshot(snapshot)
        } catch {
          // Keep last good (null on cold start). Surface loadError only when
          // there is no snapshot to bind the form to — avoids an empty Save.
          if (get().settings == null) {
            set({ loadError: true })
          }
        } finally {
          set({ loading: false })
        }
      })()

      eventDisposed = false
      void subscribe<ConversationExperienceSettings>(
        CONVERSATION_EXPERIENCE_SETTINGS_CHANGED_EVENT,
        (snapshot) => {
          get().applySnapshot(snapshot)
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

      reconnectUnsub =
        onTransportReconnect(() => {
          void get().refresh()
        }) ?? null
    },

    refresh: async () => {
      if (refreshInFlight) {
        await refreshInFlight
        return
      }
      refreshInFlight = (async () => {
        const hadSettings = get().settings != null
        // Retry / cold-start path: show loading and clear the prior error so
        // the UI does not stay on a stale failure state mid-fetch.
        if (!hadSettings) {
          set({ loading: true, loadError: false })
        }
        try {
          const snapshot = await getConversationExperienceSettings()
          get().applySnapshot(snapshot)
        } catch {
          // Retain last good snapshot on failure. Only flag loadError when
          // there is still nothing to bind the form to.
          if (get().settings == null) {
            set({ loadError: true })
          }
        } finally {
          if (!hadSettings) {
            set({ loading: false })
          }
        }
      })()
      try {
        await refreshInFlight
      } finally {
        refreshInFlight = null
      }
    },

    setAutoTitleApiConfig: async (params) => {
      const saved = await setAutoTitleApiConfigApi(params)
      get().applySnapshot(saved)
      return saved
    },

    setDocumentTranslateAgent: async (agent) => {
      const saved = await setDocumentTranslateAgentApi(agent)
      get().applySnapshot(saved)
      return saved
    },

    setReferenceSearchLimit: async (limit) => {
      const saved = await setReferenceSearchLimitApi(limit)
      get().applySnapshot(saved)
      return saved
    },
  }))

function disposeSharedSubscription(): void {
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
  initialized = false
  refreshInFlight = null
}

/**
 * Mount-time bootstrap: ref-counted so multiple consumers create one
 * subscription and the final release disposes it.
 */
export function useConversationExperienceBootstrap(): void {
  useEffect(() => {
    bootstrapRefCount += 1
    useConversationExperienceStore.getState().initialize()
    return () => {
      bootstrapRefCount -= 1
      if (bootstrapRefCount === 0) {
        disposeSharedSubscription()
      }
    }
  }, [])
}

export function resetConversationExperienceStore(): void {
  disposeSharedSubscription()
  bootstrapRefCount = 0
  useConversationExperienceStore.setState({
    settings: null,
    loading: false,
    loadError: false,
  })
}

registerBackendScopedStoreReset(resetConversationExperienceStore)
