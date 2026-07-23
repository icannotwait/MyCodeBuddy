import type { AgentType } from "@/lib/types"
import { setDelegatedChildTabIntent } from "@/lib/delegated-child-tab-intent"
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
  let agentType = input.agentType
  if (conversationId == null || conversationId <= 0) {
    return null
  }

  const summary = useAppWorkspaceStore
    .getState()
    .conversations.find((c) => c.id === conversationId)

  // Prefer explicit agentType; fall back to workspace summary for meta cards.
  if (!agentType && summary?.agent_type) {
    agentType = summary.agent_type as AgentType
  }
  if (!agentType) return null

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

/**
 * Open (or focus) a delegated child conversation as a main tab.
 *
 * Optional intent fields reproduce SubAgentSessionDialog semantics:
 * live ownership / kickoff synthesis and selected-run turn focus.
 * Intent is only staged when `openTab` is about to run (target resolved).
 */
export async function openDelegatedChildSession(input: {
  childConversationId: number | null | undefined
  agentType: AgentType | null | undefined
  title?: string | null
  kickoffTask?: string | null
  childTurnAnchor?: string | null
  /** Default true when kickoff or anchor is supplied; otherwise false. */
  liveOwnsActiveTurn?: boolean
}): Promise<boolean> {
  const target = resolveDelegatedChildOpenTarget(input)
  if (!target) return false

  const hasKickoff =
    input.kickoffTask != null && String(input.kickoffTask).trim().length > 0
  const hasAnchor =
    input.childTurnAnchor != null &&
    String(input.childTurnAnchor).trim().length > 0
  // Dialog always entered live-owned viewer mode for the child tab.
  const liveOwns = input.liveOwnsActiveTurn ?? true

  // Stage intent before openTab so a keep-alive surface can consume on focus.
  setDelegatedChildTabIntent(target.conversationId, {
    focusTurnAnchor: hasAnchor ? String(input.childTurnAnchor).trim() : null,
    kickoffTask: hasKickoff ? String(input.kickoffTask).trim() : null,
    liveOwnsActiveTurn: liveOwns,
  })

  const opened = await useTabStore
    .getState()
    .openTab(
      target.folderId,
      target.conversationId,
      target.agentType,
      false,
      target.title
    )

  if (!opened) {
    // Do not leave orphaned live-ownership intent if the tab never opened.
    const { clearDelegatedChildTabIntent } =
      await import("@/lib/delegated-child-tab-intent")
    clearDelegatedChildTabIntent(target.conversationId)
  }

  return opened
}
