"use client"

import { type ReactNode, useEffect, useMemo } from "react"
import { AlertProvider } from "@/contexts/alert-context"
import { TaskProvider } from "@/contexts/task-context"
import {
  AcpConnectionsProvider,
  useAcpActions,
} from "@/contexts/acp-connections-context"
import { ConversationRuntimeProvider } from "@/contexts/conversation-runtime-context"
import { DelegationProvider } from "@/contexts/delegation-context"
import { SessionStatsProvider } from "@/contexts/session-stats-context"
import { GitCredentialProvider } from "@/contexts/git-credential-context"
import { WorkspaceProvider } from "@/contexts/workspace-context"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"
import { useTabStore } from "@/stores/tab-store"
import type { AgentType, FolderDetail } from "@/lib/types"

/**
 * Provider tree for detached conversation window.
 * Intentionally omits TabProvider (no opened-tabs hydrate/save).
 * Synthetic tab seed is memory-only via seedDetachedSessionTab.
 */
export function DetachedShellProviders({ children }: { children: ReactNode }) {
  return (
    <AlertProvider>
      <GitCredentialProvider>
        <TaskProvider>
          <AcpConnectionsProvider>
            <SessionStatsProvider>
              <ConversationRuntimeProvider>
                <DelegationProvider>
                  <WorkspaceProvider>{children}</WorkspaceProvider>
                </DelegationProvider>
              </ConversationRuntimeProvider>
            </SessionStatsProvider>
          </AcpConnectionsProvider>
        </TaskProvider>
      </GitCredentialProvider>
    </AlertProvider>
  )
}

/** Memory-only tab seed so session surface hooks can resolve ownTab. */
export function seedDetachedSessionTab(args: {
  folderId: number
  conversationId: number
  agentType: AgentType
  workingDir?: string
  title?: string
}): string {
  const tabId = `conv-${args.folderId}-${args.agentType}-${args.conversationId}`
  const tab = {
    id: tabId,
    kind: "conversation" as const,
    folderId: args.folderId,
    conversationId: args.conversationId,
    agentType: args.agentType,
    title: args.title?.trim() || "Conversation",
    isPinned: true,
    workingDir: args.workingDir,
  }
  // Direct setState: no TabProvider hydrate/save effects run in this window.
  useTabStore.setState({
    rawTabs: [tab],
    activeTabId: tabId,
    tabs: [tab],
    tabsHydrated: true,
  })
  useAppWorkspaceStore.getState().setActiveFolderId(args.folderId)
  return tabId
}

export function seedDetachedFolder(folder: FolderDetail): void {
  useAppWorkspaceStore.getState().upsertFolder(folder)
}

/** Register this webview's open context key so idle sweep will not reap it. */
export function DetachedOpenTabKeysRegistrar({
  contextKey,
}: {
  contextKey: string
}) {
  const { registerOpenTabKeys } = useAcpActions()
  const keys = useMemo(() => new Set([contextKey]), [contextKey])
  useEffect(() => {
    registerOpenTabKeys(keys)
  }, [keys, registerOpenTabKeys])
  return null
}
