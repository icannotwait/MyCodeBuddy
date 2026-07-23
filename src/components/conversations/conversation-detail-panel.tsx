"use client"

import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react"
import {
  Copy,
  Download,
  FileCode,
  FileImage,
  FileText,
  Info,
  RefreshCw,
  SquarePen,
  X,
} from "lucide-react"
import { useTranslations } from "next-intl"
import { toast } from "sonner"
import {
  useAcpActions,
  useAcpEvent,
  useConnectionStore,
} from "@/contexts/acp-connections-context"
import { useActiveFolder } from "@/contexts/active-folder-context"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"
import { useTabActions, useTabStore } from "@/contexts/tab-context"
import { useTaskContext } from "@/contexts/task-context"
import { cn, copyTextFromMenu } from "@/lib/utils"
import { DelegationRouteMenu } from "@/components/conversations/delegation-route-menu"
import { TileScrollContainer } from "@/components/conversations/tile-scroll-container"
import {
  completeLiveTranscriptTurn,
  getConversationIdByExternalIdFromStore,
  getRuntimeSession,
  useConversationRuntimeActions,
  useConversationRuntimeStore,
} from "@/stores/conversation-runtime-store"
import type { PointerEvent as ReactPointerEvent } from "react"
import {
  type AgentType,
  type ConversationStatus,
  type EventEnvelope,
} from "@/lib/types"
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuSub,
  ContextMenuSubContent,
  ContextMenuSubTrigger,
  ContextMenuTrigger,
} from "@/components/ui/context-menu"
import {
  exportAsHtml,
  exportAsImage,
  exportAsMarkdown,
  ExportTooLongError,
} from "@/lib/export-conversation"
import { useExportLabels } from "@/lib/use-export-labels"
import { useIsMobile } from "@/hooks/use-mobile"
import { resolveActiveSessionDetails } from "./active-session-details"
import { ConversationDetailHeader } from "./conversation-detail-header"
import { SessionDetailsDialog } from "./session-details-dialog"
import { ConversationSessionSurface } from "./conversation-session-surface"

const ConversationTabView = memo(function ConversationTabView({
  tabId,
  conversationId,
  agentType,
  workingDir,
  isActive,
  showActiveFlow,
  reloadSignal,
}: {
  tabId: string
  conversationId: number | null
  agentType: AgentType
  workingDir?: string
  isActive: boolean
  showActiveFlow: boolean
  reloadSignal: number
}) {
  const ownTab = useTabStore(
    (s) => s.tabs.find((tab) => tab.id === tabId) ?? null
  )
  const folderId = ownTab?.folderId ?? 0
  return (
    <ConversationSessionSurface
      tabId={tabId}
      conversationId={conversationId}
      folderId={folderId}
      agentType={agentType}
      workingDir={workingDir}
      isActive={isActive}
      showActiveFlow={showActiveFlow}
      reloadSignal={reloadSignal}
    />
  )
})

export function ConversationDetailPanel() {
  const t = useTranslations("Folder.conversation")
  const tDetails = useTranslations("Folder.sessionDetails")
  const { removeConversation: runtimeRemoveConversation } =
    useConversationRuntimeActions()
  const { activeFolder: folder } = useActiveFolder()
  const conversations = useAppWorkspaceStore((s) => s.conversations)
  const allFolders = useAppWorkspaceStore((s) => s.allFolders)
  const tabs = useTabStore((s) => s.tabs)
  const activeTabId = useTabStore((s) => s.activeTabId)
  const isTileMode = useTabStore((s) => s.isTileMode)
  const isMobile = useIsMobile()
  const {
    openNewConversationTab,
    closeTab,
    switchTab,
    onPreviewTabReplaced,
    setDraftDelegationRoute,
  } = useTabActions()
  const { getConnection } = useConnectionStore()
  const newConversation = useMemo(() => {
    const activeTab = tabs.find((tab) => tab.id === activeTabId)
    if (!activeTab || activeTab.conversationId != null) return null
    const workingDir = activeTab.workingDir ?? folder?.path
    if (!workingDir) return null
    return { workingDir, folderId: activeTab.folderId }
  }, [tabs, activeTabId, folder?.path])
  const { disconnect: disconnectByKey } = useAcpActions()
  const { addTask, updateTask } = useTaskContext()
  const [reloadByTabId, setReloadByTabId] = useState<Record<string, number>>({})
  const [detailsOpen, setDetailsOpen] = useState(false)

  const exportLabels = useExportLabels()

  // Disconnect the old connection immediately when a preview tab is replaced
  useEffect(() => {
    return onPreviewTabReplaced((replacedTabId) => {
      disconnectByKey(replacedTabId).catch(() => {})
    })
  }, [onPreviewTabReplaced, disconnectByKey])

  // Background turn_complete handler: for conversations not open in tabs.
  // Subscribes via the context's primary `acp://event` listener (single
  // physical Tauri/WebSocket subscription, plus seq dedup from Phase 3b).
  // `useAcpEvent` stabilizes handler identity internally, so the callback
  // can read closure values directly — no caller-side refs needed.
  useAcpEvent(
    useCallback(
      (envelope: EventEnvelope) => {
        if (envelope.type !== "turn_complete") return

        const runtimeConversationId = getConversationIdByExternalIdFromStore(
          envelope.session_id
        )
        // Event-time read: fresher than a render capture ("`conversations`
        // may lag the tab update on fast turns" below applies to the render
        // snapshot; getState() narrows that window).
        const summary = useAppWorkspaceStore
          .getState()
          .conversations.find(
            (item) => item.external_id === envelope.session_id
          )
        const matchedConversationId =
          runtimeConversationId ?? summary?.id ?? null
        if (!matchedConversationId) return

        // Match against every identifier the panel may carry for the same
        // runtime session — otherwise this background handler races the
        // panel's own completeTurn effect and double-promotes streamingTurns
        // into localTurns (visible as a duplicated assistant message until
        // the conversation is reopened from DB).
        //
        // Invariant: `tab.runtimeConversationId` is only set when the panel's
        // effectiveConversationId differs from its bound conversationId, i.e.
        // for new conversations whose session lives under a virtual (negative)
        // id. `dbId2` is always a real DB id, so a runtimeConversationId vs.
        // dbId2 comparison is unreachable and intentionally omitted.
        // `conversations` may lag the tab update on fast turns, so dbId2
        // alone (without the runtime id branch) is not a reliable signal.
        const dbId2 = summary?.id
        const isOpenInTabs = tabs.some(
          (tab) =>
            tab.conversationId === matchedConversationId ||
            tab.runtimeConversationId === matchedConversationId ||
            (dbId2 != null && tab.conversationId === dbId2)
        )
        if (isOpenInTabs) return

        // Promote liveMessage + optimisticTurns to localTurns immediately,
        // coordinating the live-transcript footer handoff in the same stack.
        completeLiveTranscriptTurn(matchedConversationId)

        // If tab was closed while agent was responding, clean up now.
        // Event-time read: fresh via getState(), no reactive subscription.
        const session = getRuntimeSession(matchedConversationId)
        if (session?.pendingCleanup) {
          runtimeRemoveConversation(matchedConversationId)
        }
      },
      [tabs, runtimeRemoveConversation]
    )
  )

  const hasNoTabs = tabs.length === 0 && !activeTabId
  const activeConversationTab = useMemo(
    () =>
      tabs.find(
        (tab) => tab.id === activeTabId && tab.conversationId != null
      ) ?? null,
    [tabs, activeTabId]
  )
  const canReloadActiveConversation = activeConversationTab != null
  const handleReloadActiveConversation = useCallback(() => {
    if (!activeConversationTab) return
    setReloadByTabId((prev) => ({
      ...prev,
      [activeConversationTab.id]: (prev[activeConversationTab.id] ?? 0) + 1,
    }))
  }, [activeConversationTab])

  const [contextMenuSelectedText, setContextMenuSelectedText] = useState("")
  const savedSelectionRangeRef = useRef<Range | null>(null)
  const isContextMenuOpenRef = useRef(false)

  const handleContextMenuOpenChange = useCallback((open: boolean) => {
    isContextMenuOpenRef.current = open
    if (!open) {
      savedSelectionRangeRef.current = null
      return
    }
    const selection = window.getSelection()
    const text = selection?.toString() ?? ""
    setContextMenuSelectedText(text)
    savedSelectionRangeRef.current =
      selection && selection.rangeCount > 0 && !selection.isCollapsed
        ? selection.getRangeAt(0).cloneRange()
        : null
  }, [])

  const handleContextMenuTriggerPointerDown = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      if (event.button !== 2) return
      const selection = window.getSelection()
      if (selection && !selection.isCollapsed) {
        event.preventDefault()
      }
    },
    []
  )

  useEffect(() => {
    const handler = () => {
      if (!isContextMenuOpenRef.current) return
      const range = savedSelectionRangeRef.current
      if (!range) return
      if (
        !document.contains(range.startContainer) ||
        !document.contains(range.endContainer)
      ) {
        savedSelectionRangeRef.current = null
        return
      }
      const selection = window.getSelection()
      if (!selection) return
      if (selection.toString().length > 0) return
      selection.removeAllRanges()
      selection.addRange(range)
    }
    document.addEventListener("selectionchange", handler)
    return () => document.removeEventListener("selectionchange", handler)
  }, [])

  const handleCopySelectedText = useCallback(async () => {
    if (!contextMenuSelectedText) return
    const ok = await copyTextFromMenu(contextMenuSelectedText)
    if (ok) {
      toast.success(t("copyTextSuccess"))
    } else {
      toast.error(t("copyTextFailed"))
    }
  }, [contextMenuSelectedText, t])

  const handleNewConversation = useCallback(() => {
    if (!folder) return
    // Right-click "new conversation" inside a conversation tab: keep the
    // active agent when the target folder has no pinned default.
    openNewConversationTab(folder.id, folder.path, { inheritFromActive: true })
  }, [folder, openNewConversationTab])

  const handleCloseActiveTab = useCallback(() => {
    if (!activeTabId) return
    closeTab(activeTabId)
  }, [activeTabId, closeTab])

  // Narrow reactive reads for the ACTIVE conversation only — a background
  // conversation's streaming token no longer re-renders this panel. `canExport`
  // keys on the tab's persisted `conversationId`; the session-details
  // resolution keys on `runtimeConversationId ?? conversationId` (a brand-new
  // conversation streams under a virtual runtime id whose live stats differ), so
  // the two are subscribed SEPARATELY — collapsing them to one lookup would
  // diverge during the virtual→persisted reconciliation window.
  const activeExportConversationId =
    activeConversationTab?.conversationId ?? null
  const canExport = useConversationRuntimeStore(
    (s) =>
      activeExportConversationId != null &&
      s.byConversationId.get(activeExportConversationId)?.detail != null
  )

  // Resolve the active conversation's summary + live token usage the same way
  // the tab view renders them — a new conversation streams under a virtual
  // `runtimeConversationId` with its usage on `sessionStats`. Extracted so the
  // resolution is unit-tested (see active-session-details.test.ts).
  const activeRuntimeId =
    activeConversationTab?.runtimeConversationId ??
    activeConversationTab?.conversationId ??
    null
  const activeRuntimeSession = useConversationRuntimeStore((s) =>
    activeRuntimeId != null
      ? (s.byConversationId.get(activeRuntimeId) ?? null)
      : null
  )
  const {
    summary: activeSessionSummary,
    stats: activeSessionStats,
    model: activeSessionModel,
  } = resolveActiveSessionDetails(
    activeConversationTab,
    // resolveActiveSessionDetails reads only `getSession(runtimeId)`, and its
    // internal `runtimeId` equals `activeRuntimeId` (identical computation), so
    // resolving that single pre-selected session is exact.
    (id) => (id === activeRuntimeId ? activeRuntimeSession : null),
    conversations
  )

  const activeRouteMenuTab = useMemo(
    () => tabs.find((tab) => tab.id === activeTabId) ?? null,
    [tabs, activeTabId]
  )
  const activeRouteConnectionId =
    activeRouteMenuTab != null
      ? (getConnection(activeRouteMenuTab.id)?.connectionId ?? null)
      : null
  // Secret-safe watchdog diagnostics for Session Details (no raw tool input).
  const activeWatchdogConnection =
    activeRouteMenuTab != null ? getConnection(activeRouteMenuTab.id) : null
  const activeToolWatchdogProjections =
    activeWatchdogConnection?.toolWatchdogProjections ?? null
  const activeLastToolWatchdogDiagnostic =
    activeWatchdogConnection?.lastToolWatchdogDiagnostic ?? null

  const getExportData = useCallback(() => {
    if (!activeConversationTab?.conversationId) return null
    const session = getRuntimeSession(activeConversationTab.conversationId)
    if (!session?.detail) return null
    return {
      summary: session.detail.summary,
      turns: session.detail.turns,
      sessionStats: session.detail.session_stats,
      labels: exportLabels,
    }
  }, [activeConversationTab, exportLabels])

  const handleExportMarkdown = useCallback(async () => {
    const data = getExportData()
    if (!data) return
    try {
      const result = await exportAsMarkdown(data)
      if (result === "saved") toast.success(t("exportSuccess"))
      // "cancelled": user dismissed the Save dialog — stay silent,
      // matching the downloadImage / workspace-download conventions.
    } catch (err) {
      toast.error(t("exportFailed"))
      console.error("[ConversationDetailPanel] export markdown:", err)
    }
  }, [getExportData, t])

  const handleExportHtml = useCallback(async () => {
    const data = getExportData()
    if (!data) return
    try {
      const result = await exportAsHtml(data)
      if (result === "saved") toast.success(t("exportSuccess"))
    } catch (err) {
      toast.error(t("exportFailed"))
      console.error("[ConversationDetailPanel] export html:", err)
    }
  }, [getExportData, t])

  const handleExportImage = useCallback(async () => {
    const data = getExportData()
    if (!data) return
    const taskId = `export-image-${Date.now()}`
    addTask(taskId, t("exportImage"))
    updateTask(taskId, { status: "running" })
    try {
      const result = await exportAsImage(data)
      updateTask(taskId, { status: "completed" })
      if (result === "saved") toast.success(t("exportSuccess"))
    } catch (err) {
      updateTask(taskId, { status: "failed" })
      if (err instanceof ExportTooLongError) {
        toast.error(t("exportImageTooLong"))
      } else {
        toast.error(t("exportFailed"))
      }
      console.error("[ConversationDetailPanel] export image:", err)
    }
  }, [getExportData, t, addTask, updateTask])

  // Ensure no-tab state is immediately bridged to a real new-conversation tab.
  useEffect(() => {
    if (!folder) return

    if (hasNoTabs) {
      openNewConversationTab(
        folder.id,
        newConversation?.workingDir ?? folder.path
      )
    }
  }, [folder, hasNoTabs, newConversation?.workingDir, openNewConversationTab])

  const canTile = isTileMode && tabs.length > 1

  const tileTabRefs = useRef<Map<string, HTMLDivElement | null>>(new Map())

  useEffect(() => {
    if (!canTile || !activeTabId) return
    const el = tileTabRefs.current.get(activeTabId)
    if (!el) return
    el.scrollIntoView({
      behavior: "smooth",
      inline: "center",
      block: "nearest",
    })
  }, [canTile, activeTabId])

  if (hasNoTabs) {
    return null
  }

  const tabElements = tabs.map((tab, index) => {
    const active = tab.id === activeTabId
    const folderPath = allFolders.find((f) => f.id === tab.folderId)?.path
    const view = (
      <ConversationTabView
        tabId={tab.id}
        conversationId={tab.conversationId}
        agentType={tab.agentType}
        workingDir={tab.workingDir ?? folderPath}
        isActive={active}
        showActiveFlow={canTile && active}
        reloadSignal={reloadByTabId[tab.id] ?? 0}
      />
    )
    return (
      <div
        key={tab.id}
        ref={(el) => {
          if (el) {
            tileTabRefs.current.set(tab.id, el)
          } else {
            tileTabRefs.current.delete(tab.id)
          }
        }}
        className={cn(
          canTile
            ? cn(
                "relative h-full min-w-[24rem] flex-1 overflow-hidden",
                index > 0 && "border-l border-border/50"
              )
            : active
              ? "h-full"
              : "absolute inset-0 invisible pointer-events-none"
        )}
        onPointerDownCapture={
          canTile && !active ? () => switchTab(tab.id) : undefined
        }
      >
        {/* The visible active cue is now the composer's flowing gradient border
            (see message-input.tsx); keep a non-visual cue for assistive tech in
            tiled mode, where the old top-center icon used to provide it. */}
        {canTile && active && (
          <span className="sr-only">{t("activeConversationIndicator")}</span>
        )}
        {view}
      </div>
    )
  })

  // A single header (desktop only) sits fixed above the horizontally-scrolling
  // tile row, so it never scrolls on the x-axis when conversations are tiled.
  // It reflects the ACTIVE conversation (title + owning folder).
  const activeTab = tabs.find((tab) => tab.id === activeTabId) ?? null
  const activeTabFolder = activeTab
    ? allFolders.find((f) => f.id === activeTab.folderId)
    : undefined

  return (
    <>
      <div className="flex h-full min-h-0 flex-col overflow-hidden">
        {!isMobile && activeTab && (
          <ConversationDetailHeader
            tabId={activeTab.id}
            conversationId={activeTab.conversationId}
            runtimeConversationId={activeTab.runtimeConversationId ?? null}
            folderId={activeTab.folderId}
            folderPath={activeTabFolder?.path}
            folderName={activeTabFolder?.name ?? null}
            folderAlias={activeTabFolder?.alias ?? null}
            title={activeTab.title}
            status={activeTab.status as ConversationStatus | undefined}
          />
        )}
        <ContextMenu onOpenChange={handleContextMenuOpenChange}>
          <ContextMenuTrigger asChild>
            <div
              className="relative min-h-0 flex-1 overflow-hidden"
              onPointerDown={handleContextMenuTriggerPointerDown}
            >
              {/* Stable wrapper across canTile flip — otherwise sibling tabs remount and a live streaming response is torn down. */}
              <TileScrollContainer canTile={canTile}>
                <div
                  className={cn(
                    "relative h-full",
                    canTile && "flex min-w-full flex-row"
                  )}
                >
                  {tabElements}
                </div>
              </TileScrollContainer>
            </div>
          </ContextMenuTrigger>
          <ContextMenuContent>
            <ContextMenuItem
              disabled={!contextMenuSelectedText}
              onSelect={handleCopySelectedText}
            >
              <Copy className="h-4 w-4" />
              {t("copyText")}
            </ContextMenuItem>
            <ContextMenuSeparator />
            <ContextMenuItem
              disabled={!folder?.path}
              onSelect={handleNewConversation}
            >
              <SquarePen className="h-4 w-4" />
              {t("newConversation")}
            </ContextMenuItem>
            <ContextMenuSub>
              <ContextMenuSubTrigger disabled={!canExport}>
                <Download className="h-4 w-4" />
                {t("exportConversation")}
              </ContextMenuSubTrigger>
              <ContextMenuSubContent>
                <ContextMenuItem onSelect={handleExportImage}>
                  <FileImage className="h-4 w-4" />
                  {t("exportImage")}
                </ContextMenuItem>
                <ContextMenuItem onSelect={handleExportMarkdown}>
                  <FileText className="h-4 w-4" />
                  {t("exportMarkdown")}
                </ContextMenuItem>
                <ContextMenuItem onSelect={handleExportHtml}>
                  <FileCode className="h-4 w-4" />
                  {t("exportHtml")}
                </ContextMenuItem>
              </ContextMenuSubContent>
            </ContextMenuSub>
            <ContextMenuItem
              disabled={!canReloadActiveConversation}
              onSelect={handleReloadActiveConversation}
            >
              <RefreshCw className="h-4 w-4" />
              {t("reload")}
            </ContextMenuItem>
            <ContextMenuItem
              disabled={!activeSessionSummary}
              onSelect={() => setDetailsOpen(true)}
            >
              <Info className="h-4 w-4" />
              {tDetails("menuLabel")}
            </ContextMenuItem>
            {activeRouteMenuTab ? (
              <DelegationRouteMenu
                agentType={activeRouteMenuTab.agentType}
                conversationId={activeRouteMenuTab.conversationId}
                parentId={activeSessionSummary?.parent_id}
                connectionId={activeRouteConnectionId}
                value={
                  activeRouteMenuTab.conversationId != null
                    ? (activeSessionSummary?.delegation_route_override ?? null)
                    : (activeRouteMenuTab.delegationRouteOverride ?? null)
                }
                onDraftChange={(value) =>
                  setDraftDelegationRoute(activeRouteMenuTab.id, value)
                }
              />
            ) : null}
            <ContextMenuSeparator />
            <ContextMenuItem
              disabled={!activeTabId}
              onSelect={handleCloseActiveTab}
            >
              <X className="h-4 w-4" />
              {t("closeConversation")}
            </ContextMenuItem>
          </ContextMenuContent>
        </ContextMenu>
      </div>
      {activeSessionSummary && (
        <SessionDetailsDialog
          open={detailsOpen}
          onOpenChange={setDetailsOpen}
          summary={activeSessionSummary}
          stats={activeSessionStats}
          model={activeSessionModel}
          toolWatchdogProjections={activeToolWatchdogProjections}
          lastToolWatchdogDiagnostic={activeLastToolWatchdogDiagnostic}
        />
      )}
    </>
  )
}
