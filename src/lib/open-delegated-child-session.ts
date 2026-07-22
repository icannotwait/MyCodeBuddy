import type { AgentType } from "@/lib/types"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"
import { useTabStore } from "@/stores/tab-store"

export type DelegatedChildOpenTarget = {
  folderId: number
  conversationId: number
  agentType: AgentType
  title?: string
}

export function resolveDelegatedChildOpenTarget(input: {
  childConversationId: number | null | undefined
  agentType: AgentType | null | undefined
  title?: string | null
}): DelegatedChildOpenTarget | null {
  const conversationId = input.childConversationId
  const agentType = input.agentType
  if (conversationId == null || conversationId <= 0 || !agentType) {
    return null
  }

  const summary = useAppWorkspaceStore
    .getState()
    .conversations.find((c) => c.id === conversationId)

  let folderId = summary?.folder_id
  if (folderId == null || folderId <= 0) {
    const tabState = useTabStore.getState()
    const active = tabState.rawTabs.find((t) => t.id === tabState.activeTabId)
    folderId = active?.folderId
  }
  if (folderId == null || folderId <= 0) return null

  const title =
    (summary?.title && summary.title.trim()) ||
    (input.title && input.title.trim()) ||
    undefined

  return { folderId, conversationId, agentType, title }
}

export async function openDelegatedChildSession(input: {
  childConversationId: number | null | undefined
  agentType: AgentType | null | undefined
  title?: string | null
}): Promise<boolean> {
  const target = resolveDelegatedChildOpenTarget(input)
  if (!target) return false
  return useTabStore
    .getState()
    .openTab(
      target.folderId,
      target.conversationId,
      target.agentType,
      false,
      target.title
    )
}
