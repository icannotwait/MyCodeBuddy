"use client"

import { useCallback, useMemo, useSyncExternalStore } from "react"
import { useShallow } from "zustand/react/shallow"
import { useTranslations } from "next-intl"
import { useConnectionStore } from "@/contexts/acp-connections-context"
import { useTabStore } from "@/contexts/tab-context"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"
import { useConversationRuntimeStore } from "@/stores/conversation-runtime-store"
import {
  resolveActiveSessionDetails,
  type RuntimeSessionForDetails,
} from "@/components/conversations/active-session-details"
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip"
import type { MessageTurn } from "@/lib/types"
import { resolveSessionModelDisplay } from "@/lib/status-bar-session-model"

// Stable empty-turns reference so a shallow runtime slice stays reference-equal
// when the active tab has no session (avoids re-renders on unrelated streaming).
const EMPTY_TURNS: MessageTurn[] = []

/**
 * Bottom status-bar chip: the active conversation's model and thinking level.
 *
 * Model may fall back to live session config; **effort is history-only**
 * (Codex `turn_context.effort`, Grok summary `reasoning_effort` on turns).
 * If turns have no effort, nothing is shown for thinking level.
 */
export function StatusBarSessionModel() {
  const t = useTranslations("Folder.statusBar.sessionModel")
  const store = useConnectionStore()
  const tabs = useTabStore((s) => s.tabs)
  const activeTabId = useTabStore((s) => s.activeTabId)

  const activeConversationTab = useMemo(() => {
    const tab = tabs.find((item) => item.id === activeTabId)
    if (!tab || tab.kind !== "conversation") return null
    return tab
  }, [tabs, activeTabId])

  const contextKey = activeConversationTab?.id ?? null

  const subscribeConn = useCallback(
    (cb: () => void) => {
      if (!contextKey) return () => {}
      return store.subscribeKey(contextKey, cb)
    },
    [store, contextKey]
  )
  const getConnSnapshot = useCallback(
    () => (contextKey ? store.getConnection(contextKey) : undefined),
    [store, contextKey]
  )
  const conn = useSyncExternalStore(
    subscribeConn,
    getConnSnapshot,
    getConnSnapshot
  )

  const sessionConfigOptions =
    conn &&
    activeConversationTab &&
    conn.agentType === activeConversationTab.agentType
      ? conn.configOptions
      : null

  const activeRuntimeId =
    activeConversationTab?.runtimeConversationId ??
    activeConversationTab?.conversationId ??
    null

  const runtimeSlice = useConversationRuntimeStore(
    useShallow((s) => {
      const session =
        activeRuntimeId != null
          ? s.byConversationId.get(activeRuntimeId)
          : undefined
      return {
        detail: session?.detail ?? null,
        sessionStats: session?.sessionStats ?? null,
        localTurns: session?.localTurns ?? EMPTY_TURNS,
      } satisfies RuntimeSessionForDetails
    })
  )

  const conversations = useAppWorkspaceStore((s) => s.conversations)

  const { conversationModel, conversationEffort } = useMemo(() => {
    if (!activeConversationTab) {
      return { conversationModel: null, conversationEffort: null }
    }
    const details = resolveActiveSessionDetails(
      activeConversationTab,
      (id) => (id === activeRuntimeId ? runtimeSlice : null),
      conversations
    )
    return {
      conversationModel: details.model,
      conversationEffort: details.reasoningEffort,
    }
  }, [activeConversationTab, activeRuntimeId, runtimeSlice, conversations])

  const { model, thinkingLevel } = resolveSessionModelDisplay({
    configOptions: sessionConfigOptions,
    conversationModel,
    conversationEffort,
  })

  if (!model && !thinkingLevel) return null

  const tooltip =
    model && thinkingLevel
      ? t("tooltipBoth", { model, thinking: thinkingLevel })
      : model
        ? t("tooltipModel", { model })
        : t("tooltipThinking", { thinking: thinkingLevel! })

  return (
    <TooltipProvider>
      <Tooltip>
        <TooltipTrigger asChild>
          <div
            className="flex min-w-0 max-w-[18rem] items-center gap-1.5"
            aria-label={tooltip}
          >
            {model && <span className="truncate">{model}</span>}
            {model && thinkingLevel && (
              <span className="shrink-0 text-muted-foreground/50" aria-hidden>
                ·
              </span>
            )}
            {thinkingLevel && (
              <span className="shrink-0 truncate">{thinkingLevel}</span>
            )}
          </div>
        </TooltipTrigger>
        <TooltipContent side="top" align="start">
          {tooltip}
        </TooltipContent>
      </Tooltip>
    </TooltipProvider>
  )
}
