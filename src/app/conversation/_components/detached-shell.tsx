"use client"

import { type ReactNode, useEffect, useMemo } from "react"
import { AlertProvider } from "@/contexts/alert-context"
import { AppWorkspaceProvider } from "@/contexts/app-workspace-context"
import { TaskProvider } from "@/contexts/task-context"
import {
  AcpConnectionsProvider,
  useAcpActions,
} from "@/contexts/acp-connections-context"
import { ConversationRuntimeProvider } from "@/contexts/conversation-runtime-context"
import { DelegationProvider } from "@/contexts/delegation-context"
import { GitCredentialProvider } from "@/contexts/git-credential-context"
import { WorkbenchRouteProvider } from "@/contexts/workbench-route-context"
import { WorkspaceProvider } from "@/contexts/workspace-context"
import type {
  AgentType,
  DbConversationSummary,
  FolderDetail,
} from "@/lib/types"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"
import { useTabStore } from "@/stores/tab-store"

/**
 * Provider tree for detached conversation window.
 * Intentionally omits TabProvider (no opened-tabs hydrate/save).
 * Synthetic tab seed is memory-only via seedDetachedSessionTab.
 */
export function DetachedShellProviders({ children }: { children: ReactNode }) {
  return (
    <AppWorkspaceProvider>
      <AlertProvider>
        <GitCredentialProvider>
          <TaskProvider>
            <AcpConnectionsProvider>
              <ConversationRuntimeProvider>
                <DelegationProvider>
                  <WorkspaceProvider>
                    <WorkbenchRouteProvider>{children}</WorkbenchRouteProvider>
                  </WorkspaceProvider>
                </DelegationProvider>
              </ConversationRuntimeProvider>
            </AcpConnectionsProvider>
          </TaskProvider>
        </GitCredentialProvider>
      </AlertProvider>
    </AppWorkspaceProvider>
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
  const store = useAppWorkspaceStore.getState()
  store.upsertFolder(folder)
  if (folder.git_branch != null) {
    store.setBranch(folder.id, folder.git_branch)
  }
}

export function seedDetachedConversationSummary(
  summary: DbConversationSummary
): void {
  useAppWorkspaceStore.getState().applyConversationUpsert(summary)
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
