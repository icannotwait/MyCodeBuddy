"use client"

import { useEffect, useReducer, useRef, useSyncExternalStore } from "react"
import { useShallow } from "zustand/react/shallow"
import { useWorkbenchRoute } from "@/contexts/workbench-route-context"
import { useWorkspaceView } from "@/contexts/workspace-context"
import { useTabStore } from "@/contexts/tab-context"
import { clearAwaitingReply } from "@/lib/api"
import { onTransportReconnect } from "@/lib/platform"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"

function subscribeDocumentActivity(notify: () => void): () => void {
  window.addEventListener("focus", notify)
  window.addEventListener("blur", notify)
  document.addEventListener("visibilitychange", notify)
  return () => {
    window.removeEventListener("focus", notify)
    window.removeEventListener("blur", notify)
    document.removeEventListener("visibilitychange", notify)
  }
}

function getDocumentActivity(): boolean {
  return document.visibilityState === "visible" && document.hasFocus()
}

function getServerDocumentActivity(): boolean {
  return false
}

/**
 * Always-mounted observer: when the active persisted conversation is genuinely
 * visible (hydrated active tab, conversations route, non-maximized files,
 * document visible + focused), CAS-clear its awaiting-reply token once per
 * exact (conversationId, token). Does not clear inactive tiles or background
 * details getters — only the global active tab qualifies.
 */
export function ConversationAwaitingReplyClearer() {
  const { activeTabId, tabsHydrated, activeConversationId } = useTabStore(
    useShallow((state) => {
      const active = state.tabs.find((tab) => tab.id === state.activeTabId)
      return {
        activeTabId: state.activeTabId,
        tabsHydrated: state.tabsHydrated,
        activeConversationId: active?.conversationId ?? null,
      }
    })
  )
  const token = useAppWorkspaceStore((state) =>
    activeConversationId == null
      ? null
      : (state.conversations.find((item) => item.id === activeConversationId)
          ?.awaiting_reply_token ?? null)
  )
  const applyPatch = useAppWorkspaceStore(
    (state) => state.applyConversationStatePatch
  )
  const { isConversations } = useWorkbenchRoute()
  const { filesMaximized } = useWorkspaceView()
  const documentActive = useSyncExternalStore(
    subscribeDocumentActivity,
    getDocumentActivity,
    getServerDocumentActivity
  )
  const inFlight = useRef(new Set<string>())
  // Monotonic reconnect generation: a reconnect while the same key is in-flight
  // is not lost when the effect early-returns on inFlight; finally re-bumps retry.
  const reconnectGeneration = useRef(0)
  const [retryEpoch, requestRetry] = useReducer((value: number) => value + 1, 0)

  useEffect(() => {
    const offReconnect = onTransportReconnect(() => {
      reconnectGeneration.current += 1
      requestRetry()
    })
    return () => offReconnect?.()
  }, [])

  useEffect(() => {
    if (!tabsHydrated || !activeTabId || activeConversationId == null || !token)
      return
    if (!isConversations || filesMaximized || !documentActive) return
    const key = `${activeConversationId}:${token}`
    if (inFlight.current.has(key)) return
    inFlight.current.add(key)
    const startedGeneration = reconnectGeneration.current
    void clearAwaitingReply(activeConversationId, token)
      .then(applyPatch)
      .catch((error) => {
        console.warn("[AwaitingReply] clear failed", error)
      })
      .finally(() => {
        inFlight.current.delete(key)
        // Only schedule another attempt when a reconnect arrived during this
        // request. Ordinary failure with no reconnect must not loop.
        if (reconnectGeneration.current !== startedGeneration) {
          requestRetry()
        }
      })
  }, [
    activeConversationId,
    activeTabId,
    applyPatch,
    documentActive,
    filesMaximized,
    isConversations,
    retryEpoch,
    tabsHydrated,
    token,
  ])

  return null
}
