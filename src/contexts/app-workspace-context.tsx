"use client"

import { useEffect, type ReactNode } from "react"
import { getGitHead } from "@/lib/api"
import { selectAcpAgentsFresh, useAcpAgents } from "@/hooks/use-acp-agents"
import { delegationChildProjectionCache } from "@/lib/delegation-child-projection-cache"
import { onTransportReconnect, subscribe } from "@/lib/platform"
import { referenceSearchCache } from "@/lib/reference-search-cache"
import { getActiveBackendCacheKey } from "@/lib/transport"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"
import { useConversationExperienceBootstrap } from "@/stores/conversation-experience-store"
import {
  useDelegationProfileBootstrap,
  useDelegationProfileStore,
} from "@/stores/delegation-profile-store"
import { useConversationRuntimeStore } from "@/stores/conversation-runtime-store"
import {
  CONVERSATIONS_BULK_CHANGED_EVENT,
  CONVERSATION_CHANGED_EVENT,
  FOLDER_CHANGED_EVENT,
  type ConversationChange,
  type ConversationsBulkChanged,
  type FolderChange,
} from "@/lib/types"

interface AppWorkspaceProviderProps {
  children: ReactNode
}

/**
 * True once ACP agents are fresh and the profile catalog store has finished
 * its first bootstrap attempt (success or error). A profile error still
 * permits agent-only mentions because `ready` latches true on failure.
 */
export function selectReferenceCatalogReady(): boolean {
  return selectAcpAgentsFresh() && useDelegationProfileStore.getState().ready
}

/**
 * On a cross-client `conversation://changed` nudge, poll the persisted detail of
 * a conversation THIS client is passively viewing back into sync (its just-
 * completed reply, streamed to whoever owns the agent, isn't on our live path).
 * The runtime store no-ops unless the conversation is open and this client is a
 * pure viewer of it — the owner's in-memory reply is never refetched over.
 */
function syncOpenViewerDetail(conversationId: number): void {
  useConversationRuntimeStore
    .getState()
    .actions.syncViewerDetail(conversationId)
}

/** Terminal child task statuses that own transcript convergence (not generic status). */
const TERMINAL_DELEGATE_TASK_STATES = new Set([
  "completed",
  "failed",
  "canceled",
])

function isTerminalDelegateTaskStatus(
  status: string | null | undefined
): boolean {
  return status != null && TERMINAL_DELEGATE_TASK_STATES.has(status)
}

/**
 * Route a verified terminal delegate summary through the dedicated convergence
 * poll. Child task field owns the decision — not generic conversation status.
 * Roots keep the existing viewer-sync path; do not call both for the same child.
 */
function syncTerminalDelegateSummary(summary: {
  id: number
  kind?: string | null
  delegation_task_status?: string | null
}): void {
  if (
    summary.kind === "delegate" &&
    isTerminalDelegateTaskStatus(summary.delegation_task_status)
  ) {
    useConversationRuntimeStore
      .getState()
      .actions.syncDelegateTerminalDetail(summary.id)
  }
}

/**
 * On transport reconnect: preserve-live detail refresh for every open delegate
 * (running or terminal), plus terminal convergence only when loaded detail
 * already shows a terminal task status. Access refresh (Task 3) and observer
 * cold-attach (Task 5) are owned elsewhere.
 */
function refreshOpenDelegateDetailsOnReconnect(): void {
  const { byConversationId, actions } = useConversationRuntimeStore.getState()
  for (const [conversationId, session] of byConversationId) {
    const summary = session.detail?.summary
    if (summary?.kind !== "delegate") continue
    actions.refetchDetail(conversationId, { preserveLive: true })
    if (isTerminalDelegateTaskStatus(summary.delegation_task_status)) {
      actions.syncDelegateTerminalDetail(conversationId)
    }
  }
}

/**
 * Event wiring for `useAppWorkspaceStore` — state itself lives in the store
 * (components subscribe to slices via selectors, not through a context value).
 * This component owns the pieces that need a React lifecycle: initial fetches,
 * the cross-client side-channel subscriptions, and the active-folder branch
 * poll.
 */
export function AppWorkspaceProvider({ children }: AppWorkspaceProviderProps) {
  useConversationExperienceBootstrap()
  // Keep the shared ACP agent subscription alive for reference search, and
  // bootstrap the revisioned profile catalog outside mention opening.
  useAcpAgents()
  useDelegationProfileBootstrap()

  useEffect(() => {
    const { fetchFolders, refreshConversations } =
      useAppWorkspaceStore.getState()
    void fetchFolders()
    void refreshConversations()
  }, [])

  // Subscribe to the global `conversation://changed` side-channel so any
  // client's create/rename/delete/status reaches this client's sidebar in real
  // time — independent of whether the conversation is open/attached anywhere.
  useEffect(() => {
    let disposed = false
    let unlisten: (() => void) | undefined

    void (async () => {
      const dispose = await subscribe<ConversationChange>(
        CONVERSATION_CHANGED_EVENT,
        (change) => {
          const store = useAppWorkspaceStore.getState()
          const backend = getActiveBackendCacheKey()
          if (change.kind === "upsert") {
            store.applyConversationUpsert(change.summary)
            referenceSearchCache.markConversationUpsert(backend, change.summary)
            // This side-channel keeps the sidebar in sync but does NOT touch an
            // open conversation's detail. If THIS client is only viewing that
            // conversation (another client owns the live agent), a turn that
            // just completed there has no live promotion path here — so pull the
            // reply from the persisted transcript. No-op unless the conversation
            // is open and this client is a pure viewer of it.
            // Terminal delegates use the dedicated convergence poll instead of
            // the pure-viewer path (live-owned children would refuse it).
            if (
              change.summary.kind === "delegate" &&
              isTerminalDelegateTaskStatus(
                change.summary.delegation_task_status
              )
            ) {
              syncTerminalDelegateSummary(change.summary)
            } else {
              syncOpenViewerDetail(change.summary.id)
            }
          } else if (change.kind === "deleted") {
            store.applyConversationRemove(change.id)
            referenceSearchCache.markConversationDelete(backend, change.id)
          } else {
            store.applyConversationStatePatch(change.patch)
            referenceSearchCache.markConversationStatus(
              backend,
              change.patch.id,
              change.patch.status
            )
            syncOpenViewerDetail(change.patch.id)
          }
          // Cold child projection for delegation cards (title + summary fields).
          // Only AppWorkspaceProvider installs this listener — see plan Task 5.
          delegationChildProjectionCache.applyConversationChange(change)
        }
      )
      if (disposed) dispose()
      else unlisten = dispose
    })()

    // Events fired while the WS was disconnected are dropped by the broadcaster
    // (receiver_count == 0). A full re-fetch on reconnect reconciles. Returns
    // null on desktop IPC (no disconnect window) → no-op there.
    const offReconnect = onTransportReconnect(() => {
      void useAppWorkspaceStore.getState().refreshConversations()
      // Refetch interest-held child projections dropped while the WS was down.
      delegationChildProjectionCache.refetchTracked()
      // Open delegates: preserve-live detail + terminal convergence when applicable.
      refreshOpenDelegateDetailsOnReconnect()
    })

    return () => {
      disposed = true
      unlisten?.()
      offReconnect?.()
    }
  }, [])

  // A batch import (the import-picker window, possibly a different window than
  // this one) announces completion with ONE `conversations://bulk-changed`
  // nudge instead of per-row upserts; answer it with a single full refetch.
  // Folder creations arrive separately on `folder://changed` below.
  useEffect(() => {
    let disposed = false
    let unlisten: (() => void) | undefined

    void (async () => {
      const dispose = await subscribe<ConversationsBulkChanged>(
        CONVERSATIONS_BULK_CHANGED_EVENT,
        () => {
          void useAppWorkspaceStore.getState().refreshConversations()
        }
      )
      if (disposed) dispose()
      else unlisten = dispose
    })()

    return () => {
      disposed = true
      unlisten?.()
    }
  }, [])

  // Subscribe to the global `folder://changed` side-channel so a folder created
  // headlessly (e.g. an automation per-run worktree) lands in this client's
  // workspace list in real time — without it, a conversation produced in that
  // worktree has no known folder to group under and never renders in the sidebar.
  // Only upserts the list (+ seeds its branch); unlike WorkspaceOpenFolderListener
  // it never opens/focuses a tab, so a background emitter can't steal focus.
  useEffect(() => {
    let disposed = false
    let unlisten: (() => void) | undefined

    void (async () => {
      const dispose = await subscribe<FolderChange>(
        FOLDER_CHANGED_EVENT,
        (change) => {
          if (change.kind === "upsert") {
            const store = useAppWorkspaceStore.getState()
            store.upsertFolder(change.folder)
            // Only seed the branch when the event actually carries one. A
            // freshly-minted worktree row stores `git_branch: null` (resolved
            // later by git-head detection), and re-broadcasting an existing root
            // must not clobber its already-known in-memory branch with null.
            if (change.folder.git_branch) {
              store.setBranch(change.folder.id, change.folder.git_branch)
            }
          }
        }
      )
      if (disposed) dispose()
      else unlisten = dispose
    })()

    // A folder created while the WS was disconnected is dropped by the
    // broadcaster (receiver_count == 0); a full folder re-fetch on reconnect
    // reconciles. Returns null on desktop IPC (no disconnect window) → no-op.
    const offReconnect = onTransportReconnect(() => {
      void useAppWorkspaceStore.getState().fetchFolders()
    })

    return () => {
      disposed = true
      unlisten?.()
      offReconnect?.()
    }
  }, [])

  // Branch polling: only poll the active folder. Subscribes to the resolved
  // path (a primitive) rather than `allFolders`, so folder-list churn doesn't
  // restart the poll — only an actual active-folder change does.
  const activeFolderId = useAppWorkspaceStore((s) => s.activeFolderId)
  const activeFolderPath = useAppWorkspaceStore((s) =>
    s.activeFolderId == null
      ? null
      : (s.allFolders.find((f) => f.id === s.activeFolderId)?.path ?? null)
  )
  useEffect(() => {
    if (activeFolderId == null || activeFolderPath == null) return
    const folderId = activeFolderId

    let cancelled = false
    let timer: ReturnType<typeof setTimeout> | null = null

    const poll = async () => {
      try {
        const head = await getGitHead(activeFolderPath)
        if (cancelled) return
        useAppWorkspaceStore.getState().applyGitHead(folderId, head)
        // Poll a repo briskly to catch branch switches; back off otherwise.
        const delay = head.is_repo ? 10_000 : 60_000
        timer = setTimeout(poll, delay)
      } catch {
        if (!cancelled) {
          timer = setTimeout(poll, 60_000)
        }
      }
    }

    void poll()

    return () => {
      cancelled = true
      if (timer) clearTimeout(timer)
    }
  }, [activeFolderId, activeFolderPath])

  return <>{children}</>
}
