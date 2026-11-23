"use client"

import {
  shouldSeedConversation,
  useFolderTabsStore,
} from "@/stores/folder-tabs-store"
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
import { FolderTabsEmptyState } from "@/components/conversations/folder-tabs-empty-state"
import { useTabActions, useTabStore } from "@/contexts/tab-context"
import {
  groupOfTab,
  shellGroupOfTab,
  useVisibleConversationLayout,
  useVisibleConversationTabs,
} from "@/stores/tab-store"
import { computeRects, leafIds } from "@/lib/tab-group-layout"
import { useTaskContext } from "@/contexts/task-context"
import { cn, copyTextFromMenu } from "@/lib/utils"
import { DelegationRouteMenu } from "@/components/conversations/delegation-route-menu"
import { useTouchSelectionMenuGuard } from "@/hooks/use-touch-selection-menu-guard"
import { TileScrollContainer } from "@/components/conversations/tile-scroll-container"
import { stableTabViewOrder } from "@/lib/tab-view-order"
import { GroupSplitHandle } from "@/components/conversations/group-split-handle"
import { OverlayHostHiddenProvider } from "@/components/ui/overlay-host-hidden"
import { TabBar } from "@/components/tabs/tab-bar"
import { TabDragGhost } from "@/components/tabs/tab-drag-ghost"
import { useSidebarContext } from "@/contexts/sidebar-context"
import { useAuxPanelContext } from "@/contexts/aux-panel-context"
import { useWorkspaceView } from "@/contexts/workspace-context"
import { useIsMobile } from "@/hooks/use-mobile"
import { usePlatform } from "@/hooks/use-platform"
import { useZoomLevel } from "@/hooks/use-appearance"
import { getFolderConversation } from "@/lib/api"
import { isWindowedDetail } from "@/lib/turn-window"
import { isDesktop } from "@/lib/platform"
import { ChromeReserve } from "@/components/layout/chrome-reserve"
import { leftChromeReserve, rightChromeReserve } from "@/lib/window-chrome"
import {
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
import {
  projectRuntimeSessionDetails,
  resolveActiveSessionDetailsFromSnapshot,
} from "./active-session-details"
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
  groupId,
  isVisible,
}: {
  tabId: string
  conversationId: number | null
  agentType: AgentType
  workingDir?: string
  isActive: boolean
  showActiveFlow: boolean
  reloadSignal: number
  groupId: string
  /** Whether this view is actually on screen: the active tab, or every member
   *  of a tiled group. A mounted-but-hidden tab keeps its runtime session
   *  alive, but it must NOT auto-fetch its own detail. Restoring a workspace
   *  with N open tabs used to issue N concurrent `get_folder_conversation`
   *  calls — each one a tail window of up to `TAIL_TURNS_DEFAULT` (120) turns,
   *  so tens of MB across a large tab set — before the user had looked at a
   *  single one of them. A hidden tab now fetches on the first frame it
   *  becomes visible, so the workspace opens fetching only the tabs on screen
   *  (one per split group, or every member of a tiled one) instead of all N.
   *  Only that first load waits: see the fetch gate in the view. */
  isVisible: boolean
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
      groupId={groupId}
      isVisible={isVisible}
    />
  )
})

const GROUP_EDGE_EPSILON = 0.1

function SplitStripCornerReserve({ side }: { side: "left" | "right" }) {
  const isMobile = useIsMobile()
  const { isOpen: sidebarOpen } = useSidebarContext()
  const { isOpen: auxOpen } = useAuxPanelContext()
  const { mode, conversationMaximized } = useWorkspaceView()
  const { isMac, isWindows, isLinux } = usePlatform()
  const { zoomLevel } = useZoomLevel()
  if (isMobile) return null
  const holdsRight = mode === "conversation" || conversationMaximized
  if (side === "right" && !holdsRight) return null
  const width =
    side === "left"
      ? sidebarOpen
        ? 0
        : leftChromeReserve(isMac && isDesktop(), zoomLevel)
      : auxOpen
        ? 0
        : rightChromeReserve(isDesktop() && (isWindows || isLinux), zoomLevel)
  return <ChromeReserve width={width} />
}

export function ConversationDetailPanel() {
  const t = useTranslations("Folder.conversation")
  const tDetails = useTranslations("Folder.sessionDetails")
  const {
    completeTurn: runtimeCompleteTurn,
    removeConversation: runtimeRemoveConversation,
  } = useConversationRuntimeActions()
  const { activeFolder: folder } = useActiveFolder()
  const conversations = useAppWorkspaceStore((s) => s.conversations)
  const allFolders = useAppWorkspaceStore((s) => s.allFolders)
  const tabs = useTabStore((s) => s.tabs)
  const tabsFollowFolder = useFolderTabsStore((s) => s.enabled)
  const visibleTabs = useVisibleConversationTabs()
  const visibleLayout = useVisibleConversationLayout()
  const visibleTabIds = useMemo(
    () => new Set(visibleTabs.map((tab) => tab.id)),
    [visibleTabs]
  )
  const activeTabId = useTabStore((s) => s.activeTabId)
  const groupLayout = useTabStore((s) => s.groupLayout)
  const groupOf = useTabStore((s) => s.groupOf)
  const groupViews = useTabStore((s) => s.groupViews)
  const groupSelection = useTabStore((s) => s.groupSelection)
  const tileByGroup = useTabStore((s) => s.tileByGroup)
  const dragOverGroupId = useTabStore((s) => s.tabDrag?.overGroupId ?? null)
  const {
    openNewConversationTab,
    closeTab,
    switchTab,
    resizeGroupSplit,
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
  const { disconnectIfIdle } = useAcpActions()
  const { addTask, updateTask } = useTaskContext()
  const [reloadByTabId, setReloadByTabId] = useState<Record<string, number>>({})
  const [detailsOpen, setDetailsOpen] = useState(false)

  const exportLabels = useExportLabels()

  // Release the old connection as soon as a preview tab is replaced (the next
  // single-click in the sidebar takes its slot) instead of waiting for a sweep.
  // Idle-gated on purpose: the replaced tab may hold a session that is still
  // working — often one the user only clicked in to watch — and disconnecting
  // an owner mid-turn kills the agent CLI, which lands in the transcript as an
  // interrupted request. Busy owners keep running; the idle sweep reclaims them
  // once they settle.
  useEffect(() => {
    return onPreviewTabReplaced((replacedTabId) => {
      disconnectIfIdle(replacedTabId).catch(() => {})
    })
  }, [onPreviewTabReplaced, disconnectIfIdle])

  // Background turn_complete handler: for conversations not open in tabs.
  // Subscribes via the context's primary `acp://event` listener (single
  // physical Tauri/WebSocket subscription, plus seq dedup). `useAcpEvent`
  // stabilizes handler identity internally, so the callback can read closure
  // values directly.
  useAcpEvent(
    useCallback(
      (envelope: EventEnvelope) => {
        if (envelope.type !== "turn_complete") return

        const runtimeConversationId = getConversationIdByExternalIdFromStore(
          envelope.session_id
        )
        const summary = useAppWorkspaceStore
          .getState()
          .conversations.find(
            (item) => item.external_id === envelope.session_id
          )
        const matchedConversationId =
          runtimeConversationId ?? summary?.id ?? null
        if (!matchedConversationId) return

        // Match every identifier the panel may carry for the same runtime
        // session, or this handler races a mounted tab's own completeTurn and
        // double-promotes the live turn.
        const dbId2 = summary?.id
        const isOpenInTabs = tabs.some(
          (tab) =>
            tab.conversationId === matchedConversationId ||
            tab.runtimeConversationId === matchedConversationId ||
            (dbId2 != null && tab.conversationId === dbId2)
        )
        if (isOpenInTabs) return

        runtimeCompleteTurn(matchedConversationId)

        const session = getRuntimeSession(matchedConversationId)
        if (session?.pendingCleanup) {
          runtimeRemoveConversation(matchedConversationId)
        }
      },
      [tabs, runtimeCompleteTurn, runtimeRemoveConversation]
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
  // A persistent metadata consumer's previous hook snapshot can outlive a
  // removed session. Select its display values before React keeps that snapshot.
  const activeRuntimeDetails = useConversationRuntimeStore(
    useShallow((s) =>
      projectRuntimeSessionDetails(
        activeRuntimeId != null ? s.byConversationId.get(activeRuntimeId) : null
      )
    )
  )
  const {
    summary: activeSessionSummary,
    stats: activeSessionStats,
    model: activeSessionModel,
  } = resolveActiveSessionDetailsFromSnapshot(
    activeConversationTab,
    activeRuntimeDetails,
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

  const getExportData = useCallback(async () => {
    if (!activeConversationTab?.conversationId) return null
    const session = getRuntimeSession(activeConversationTab.conversationId)
    if (!session?.detail) return null
    let detail = session.detail
    // The loaded detail may be a tail WINDOW (paginated loading); an export
    // must cover the whole transcript, so fetch the legacy full response on
    // demand. The window is full when it starts at offset 0.
    if (isWindowedDetail(detail) && detail.turns_offset > 0) {
      detail = await getFolderConversation(
        session.dbConversationId ?? activeConversationTab.conversationId
      )
    }
    return {
      summary: detail.summary,
      turns: detail.turns,
      sessionStats: detail.session_stats,
      labels: exportLabels,
    }
  }, [activeConversationTab, exportLabels])

  const handleExportMarkdown = useCallback(async () => {
    try {
      const data = await getExportData()
      if (!data) return
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
    try {
      const data = await getExportData()
      if (!data) return
      const result = await exportAsHtml(data)
      if (result === "saved") toast.success(t("exportSuccess"))
    } catch (err) {
      toast.error(t("exportFailed"))
      console.error("[ConversationDetailPanel] export html:", err)
    }
  }, [getExportData, t])

  const handleExportImage = useCallback(async () => {
    const taskId = `export-image-${Date.now()}`
    addTask(taskId, t("exportImage"))
    updateTask(taskId, { status: "running" })
    try {
      const data = await getExportData()
      if (!data) {
        updateTask(taskId, { status: "completed" })
        return
      }
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

    if (
      shouldSeedConversation(useFolderTabsStore.getState().enabled, hasNoTabs)
    ) {
      openNewConversationTab(
        folder.id,
        newConversation?.workingDir ?? folder.path
      )
    }
  }, [
    folder,
    hasNoTabs,
    newConversation?.workingDir,
    openNewConversationTab,
    tabsFollowFolder,
  ])

  const { groups: groupRects, handles: groupHandles } = useMemo(
    () => computeRects(visibleLayout),
    [visibleLayout]
  )
  const isSplit = leafIds(visibleLayout).length > 1
  // Every tab sits in its own view's group shell (`shellGroupOfTab`): the
  // folder views not on screen keep their shells, hidden, so a folder switch
  // reparents nothing. Shells come in id order — a fixed order whatever view
  // is on screen, so a switch never moves one (a moved node resets every
  // transcript inside it); new shells only ever insert.
  const tabsByGroup = useMemo(() => {
    const byGroup = new Map<string, typeof tabs>()
    for (const groupId of leafIds(groupLayout)) byGroup.set(groupId, [])
    const state = { rawTabs: tabs, groupOf, groupLayout, groupViews }
    for (const tab of tabs) {
      const groupId = visibleTabIds.has(tab.id)
        ? groupOfTab(groupOf, groupLayout, tab.id)
        : shellGroupOfTab(state, tab.id)
      const bucket = byGroup.get(groupId)
      if (bucket) bucket.push(tab)
      else byGroup.set(groupId, [tab])
    }
    return byGroup
  }, [tabs, groupOf, groupLayout, groupViews, visibleTabIds])
  const orderedGroupIds = useMemo(
    () => [...tabsByGroup.keys()].sort(),
    [tabsByGroup]
  )

  const tileTabRefs = useRef<Map<string, HTMLDivElement | null>>(new Map())
  const groupContainerRef = useRef<HTMLDivElement | null>(null)
  // The panel's context menu wraps the transcript, so a touch long-press that
  // selects text would also open it — modal, over the selection's copy bubble.
  const panelMenu = useTouchSelectionMenuGuard(groupContainerRef)

  const tiledSelectionKey = useMemo(
    () =>
      orderedGroupIds
        .filter(
          (groupId) =>
            tileByGroup[groupId] &&
            (tabsByGroup.get(groupId) ?? []).filter((tab) =>
              visibleTabIds.has(tab.id)
            ).length > 1 &&
            groupSelection[groupId] != null
        )
        .map((groupId) => groupSelection[groupId])
        .join("|"),
    [orderedGroupIds, tileByGroup, tabsByGroup, groupSelection, visibleTabIds]
  )
  useEffect(() => {
    if (!tiledSelectionKey) return
    for (const selectedId of tiledSelectionKey.split("|")) {
      tileTabRefs.current.get(selectedId)?.scrollIntoView({
        behavior: "smooth",
        inline: "center",
        block: "nearest",
      })
    }
  }, [tiledSelectionKey])

  useEffect(() => {
    const state = useTabStore.getState()
    if (state.tabDrag && !tabs.some((tab) => tab.id === state.tabDrag?.tabId)) {
      state.endTabDrag()
    }
  }, [tabs])

  if (hasNoTabs) {
    return tabsFollowFolder ? <FolderTabsEmptyState /> : null
  }

  const renderTabWrapper = (
    tab: (typeof tabs)[number],
    indexInGroup: number,
    groupId: string,
    canTileG: boolean
  ) => {
    const inScope = visibleTabIds.has(tab.id)
    const active = inScope && tab.id === activeTabId
    // Visible = tiled (all group members shown) or the group's selected tab.
    const visible = inScope && (canTileG || tab.id === groupSelection[groupId])
    const folderPath = allFolders.find((f) => f.id === tab.folderId)?.path
    const view = (
      <ConversationTabView
        tabId={tab.id}
        conversationId={tab.conversationId}
        agentType={tab.agentType}
        workingDir={tab.workingDir ?? folderPath}
        isActive={active}
        showActiveFlow={(isSplit || canTileG) && active}
        reloadSignal={reloadByTabId[tab.id] ?? 0}
        groupId={groupId}
        isVisible={visible}
      />
    )
    return (
      <div
        key={tab.id}
        hidden={!visible}
        ref={(el) => {
          if (el) {
            tileTabRefs.current.set(tab.id, el)
          } else {
            tileTabRefs.current.delete(tab.id)
          }
        }}
        // Tiled, everything below this wrapper is invisible to the row's
        // scrollbar (a tile clips its content), so TileScrollContainer skips
        // the layout read its observer would otherwise force per mutation.
        data-tile-scroll-item=""
        style={canTileG ? { order: indexInGroup } : undefined}
        className={cn(
          !inScope
            ? "conversation-tab-hidden absolute inset-0 invisible pointer-events-none"
            : canTileG
              ? cn(
                  "relative h-full min-w-[24rem] flex-1 overflow-hidden",
                  indexInGroup > 0 && "border-l border-border/50"
                )
              : visible
                ? "h-full"
                : "conversation-tab-hidden absolute inset-0 invisible pointer-events-none"
        )}
        onPointerDownCapture={
          visible && !active ? () => switchTab(tab.id) : undefined
        }
      >
        {/* The visible active cue is now the composer's flowing gradient border
            (see message-input.tsx); keep a non-visual cue for assistive tech in
            tiled mode, where the old top-center icon used to provide it. */}
        {(isSplit || canTileG) && active && (
          <span className="sr-only">{t("activeConversationIndicator")}</span>
        )}
        {/* A backgrounded tab is kept mounted and merely hidden (its session is
            still live), but a "查看会话" drawer opened from it portals to the
            body — so without this it went on painting over whichever tab the
            user switched to. The flag is additive, so a visible tab inside a
            covered workspace stays hidden. */}
        <OverlayHostHiddenProvider hidden={!visible}>
          {view}
        </OverlayHostHiddenProvider>
      </div>
    )
  }

  const renderGroupShell = (groupId: string) => {
    const inScope = groupRects.has(groupId)
    const rect = groupRects.get(groupId) ?? { x: 0, y: 0, w: 100, h: 100 }
    const groupTabs = tabsByGroup.get(groupId) ?? []
    const visibleGroupTabs = groupTabs.filter((tab) =>
      visibleTabIds.has(tab.id)
    )
    const canTileG = !!tileByGroup[groupId] && visibleGroupTabs.length > 1
    // Only the TOP-edge strips sit under the fixed corner overlays (the split
    // layout has no title-bar row above them) — the leftmost/rightmost of that
    // row carry the corner reserves the unsplit strip row normally provides.
    const touchesTop = rect.y <= GROUP_EDGE_EPSILON
    const touchesLeft = touchesTop && rect.x <= GROUP_EDGE_EPSILON
    const touchesRight =
      touchesTop && rect.x + rect.w >= 100 - GROUP_EDGE_EPSILON
    // The group's SELECTED tab drives its header — each split group keeps the
    // full "tabs + conversation title bar" pairing of the unsplit layout.
    const selTab =
      visibleGroupTabs.find((tab) => tab.id === groupSelection[groupId]) ??
      visibleGroupTabs[0] ??
      null
    const selTabFolder = selTab
      ? allFolders.find((item) => item.id === selTab.folderId)
      : undefined

    return (
      <div
        key={groupId}
        data-conv-group-shell={groupId}
        className="absolute flex min-h-0 flex-col overflow-hidden"
        style={{
          left: `${rect.x}%`,
          top: `${rect.y}%`,
          width: `${rect.w}%`,
          height: `${rect.h}%`,
          visibility: inScope ? undefined : "hidden",
          pointerEvents: inScope ? undefined : "none",
        }}
      >
        {isSplit && (
          <div className="flex h-10 shrink-0 items-stretch bg-muted ws-transparent-bg">
            {touchesLeft && <SplitStripCornerReserve side="left" />}
            <TabBar groupId={groupId} maximizeControl={touchesRight} />
            {touchesRight && <SplitStripCornerReserve side="right" />}
          </div>
        )}
        {isSplit && selTab && (
          <div
            className="shrink-0"
            onPointerDownCapture={() => {
              const selected = groupSelection[groupId]
              if (selected && selected !== useTabStore.getState().activeTabId) {
                switchTab(selected)
              }
            }}
          >
            <ConversationDetailHeader
              tabId={selTab.id}
              conversationId={selTab.conversationId}
              runtimeConversationId={selTab.runtimeConversationId ?? null}
              folderId={selTab.folderId}
              folderPath={selTabFolder?.path}
              title={selTab.title}
              status={selTab.status as ConversationStatus | undefined}
            />
          </div>
        )}
        <div className="relative min-h-0 flex-1 overflow-hidden">
          <TileScrollContainer canTile={canTileG}>
            <div
              className={cn(
                "relative h-full",
                canTileG && "flex min-w-full flex-row"
              )}
            >
              {/* Strip order reaches the screen only through CSS `order`:
                  a reorder that moved these nodes would reset each moved
                  transcript's scroll offset and blank it (see
                  stableTabViewOrder). */}
              {stableTabViewOrder(groupTabs).map(({ tab, visualIndex }) =>
                renderTabWrapper(tab, visualIndex, groupId, canTileG)
              )}
            </div>
          </TileScrollContainer>
          {dragOverGroupId === groupId && (
            <div className="pointer-events-none absolute inset-0 z-30 bg-primary/5 ring-2 ring-inset ring-primary/30" />
          )}
        </div>
      </div>
    )
  }

  // While unsplit the active conversation keeps the shared global header.
  const activeTab = tabs.find((tab) => tab.id === activeTabId) ?? null
  const activeTabFolder = activeTab
    ? allFolders.find((f) => f.id === activeTab.folderId)
    : undefined

  return (
    <>
      <div className="flex h-full min-h-0 flex-col overflow-hidden">
        {!isSplit && activeTab && (
          <ConversationDetailHeader
            tabId={activeTab.id}
            conversationId={activeTab.conversationId}
            runtimeConversationId={activeTab.runtimeConversationId ?? null}
            folderId={activeTab.folderId}
            folderPath={activeTabFolder?.path}
            title={activeTab.title}
            status={activeTab.status as ConversationStatus | undefined}
          />
        )}
        <ContextMenu
          open={panelMenu.open}
          onOpenChange={(open) => {
            panelMenu.onOpenChange(open)
            handleContextMenuOpenChange(open)
          }}
        >
          <ContextMenuTrigger asChild>
            <div
              ref={groupContainerRef}
              className="relative min-h-0 flex-1 overflow-hidden"
              onPointerDown={handleContextMenuTriggerPointerDown}
              onPointerDownCapture={panelMenu.onPointerDownCapture}
            >
              {orderedGroupIds.map((groupId) => renderGroupShell(groupId))}
              {/* The handles resize the on-screen view's own layout (a
                  folder's, while tabs follow the folder), which the visible
                  one only differs from before the tab set is known. */}
              {isSplit &&
                visibleLayout === groupLayout &&
                groupHandles.map((handle) => (
                  <GroupSplitHandle
                    key={`${handle.splitId}:${handle.index}`}
                    handle={handle}
                    containerRef={groupContainerRef}
                    onResize={resizeGroupSplit}
                  />
                ))}
              {/* The selected folder has nothing open: every shell above is
                  hidden, other folders' live tabs included. */}
              {tabsFollowFolder && visibleTabs.length === 0 && (
                <div className="absolute inset-0">
                  <FolderTabsEmptyState />
                </div>
              )}
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
      <TabDragGhost />
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
