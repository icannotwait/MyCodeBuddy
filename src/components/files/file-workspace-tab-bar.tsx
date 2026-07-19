"use client"

import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react"
import { Reorder } from "motion/react"
import {
  Code,
  Eye,
  ExternalLink,
  FileText,
  GitCompare,
  Languages,
  Loader2,
  Maximize2,
  Minimize2,
  Save,
  X,
} from "lucide-react"
import { useLocale, useTranslations } from "next-intl"
import { toast } from "sonner"
import { openPath } from "@/lib/platform"
import { isHtmlPreviewable } from "@/lib/language-detect"
import { useActiveFolder } from "@/contexts/active-folder-context"
import {
  useWorkspaceActions,
  useWorkspaceFileTabs,
  useWorkspaceView,
} from "@/contexts/workspace-context"
import type { FileWorkspaceTab } from "@/contexts/workspace-context"
import { useIsCoarsePointer } from "@/hooks/use-is-coarse-pointer"
import { useIsMobile } from "@/hooks/use-mobile"
import { useLongPressDrag } from "@/hooks/use-long-press-drag"
import { useShortcutSettings } from "@/hooks/use-shortcut-settings"
import { matchShortcutEvent } from "@/lib/keyboard-shortcuts"
import { cn, handleMiddleClickClose } from "@/lib/utils"
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuTrigger,
} from "@/components/ui/context-menu"
import { saveTranslationAs, translateDocument } from "@/lib/api"
import {
  formatFromTranslatablePath,
  hashDocumentContent,
  intlLocaleToWire,
  isTranslationEligible,
} from "@/lib/document-translate"
import {
  extractAppCommandError,
  toLocalizedErrorMessage,
  type AppErrorTranslator,
} from "@/lib/app-error"
import { useConversationExperienceStore } from "@/stores/conversation-experience-store"

/** Escape close gate for files chrome (export for unit tests). */
export function shouldHandleFilesEscape(
  event: KeyboardEvent,
  ctx: {
    mode: string
    activePane: string
    filesMaximized: boolean
    activeFileTabId: string | null
  }
): boolean {
  if (event.key !== "Escape") return false
  if (event.defaultPrevented) return false
  if (ctx.mode !== "fusion") return false
  if (!(ctx.activePane === "files" || ctx.filesMaximized)) return false
  if (!ctx.activeFileTabId) return false

  // Overlay guard: open modal/dialog or focus inside portaled menus.
  if (typeof document !== "undefined") {
    if (
      document.querySelector('[role="dialog"][data-state="open"]') ||
      document.querySelector('[role="alertdialog"]')
    ) {
      return false
    }
    const active = document.activeElement
    if (
      active instanceof Element &&
      active.closest(
        "[data-radix-popper-content-wrapper], [data-radix-menu-content], [data-radix-dropdown-menu-content], [role='menu']"
      )
    ) {
      return false
    }
  }

  return true
}

export function FileWorkspaceTabBar() {
  const t = useTranslations("Folder.fileWorkspace")
  const tRoot = useTranslations()
  const intlLocale = useLocale()
  const { mode, activePane, filesMaximized } = useWorkspaceView()
  const { fileTabs, activeFileTabId, previewFileTabIds } =
    useWorkspaceFileTabs()
  const {
    switchFileTab,
    closeFileTab,
    closeOtherFileTabs,
    closeAllFileTabs,
    reorderFileTabs,
    toggleFileTabPreview,
    toggleFilesMaximized,
    beginTranslateRequest,
    openTranslationResultTab,
    openFilePreview,
  } = useWorkspaceActions()
  const { activeFolder } = useActiveFolder()
  const autoTitleAgent = useConversationExperienceStore(
    (s) => s.settings?.auto_title_agent ?? null
  )
  const { shortcuts } = useShortcutSettings()
  const scrollRef = useRef<HTMLDivElement>(null)
  const isCoarsePointer = useIsCoarsePointer()
  const isMobile = useIsMobile()
  const [isHovered, setIsHovered] = useState(false)
  const [touchSortingTabId, setTouchSortingTabId] = useState<string | null>(
    null
  )
  // In-flight translate guard (button busy + double-click).
  const [translateBusy, setTranslateBusy] = useState(false)
  const translateBusyRef = useRef(false)
  const [saveAsBusy, setSaveAsBusy] = useState(false)
  const saveAsBusyRef = useRef(false)

  const handleWheel = useCallback((e: React.WheelEvent<HTMLDivElement>) => {
    if (e.deltaY !== 0 && scrollRef.current) {
      e.preventDefault()
      scrollRef.current.scrollLeft += e.deltaY
    }
  }, [])

  useEffect(() => {
    if (!activeFileTabId || !scrollRef.current) return
    const el = scrollRef.current.querySelector(
      `[data-file-tab-id="${activeFileTabId}"]`
    )
    el?.scrollIntoView({ block: "nearest", inline: "nearest" })
  }, [activeFileTabId])

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      // While maximized only the files pane is interactive, so route shortcuts
      // here regardless of the user's last-clicked pane.
      const shouldHandleShortcut =
        mode === "fusion" && (activePane === "files" || filesMaximized)
      if (!shouldHandleShortcut) return
      if (matchShortcutEvent(event, shortcuts.close_all_file_tabs)) {
        event.preventDefault()
        closeAllFileTabs()
        return
      }
      if (matchShortcutEvent(event, shortcuts.close_current_tab)) {
        if (!activeFileTabId) return
        event.preventDefault()
        closeFileTab(activeFileTabId)
        return
      }

      // Fixed Escape binding (not rebindable): close active file tab with
      // overlay precedence. Independent of close_current_tab shortcut.
      if (
        shouldHandleFilesEscape(event, {
          mode,
          activePane,
          filesMaximized,
          activeFileTabId,
        }) &&
        activeFileTabId
      ) {
        event.preventDefault()
        closeFileTab(activeFileTabId)
      }
    }

    window.addEventListener("keydown", onKeyDown)
    return () => {
      window.removeEventListener("keydown", onKeyDown)
    }
  }, [
    activeFileTabId,
    closeAllFileTabs,
    closeFileTab,
    mode,
    activePane,
    filesMaximized,
    shortcuts.close_all_file_tabs,
    shortcuts.close_current_tab,
  ])

  const handleReorder = useCallback(
    (nextTabs: FileWorkspaceTab[]) => {
      if (isCoarsePointer && !touchSortingTabId) return
      reorderFileTabs(nextTabs)
    },
    [isCoarsePointer, reorderFileTabs, touchSortingTabId]
  )

  const handleTouchSortingEnd = useCallback(
    () => setTouchSortingTabId(null),
    []
  )

  const activeTab = fileTabs.find((tab) => tab.id === activeFileTabId)
  const canPreview =
    activeTab?.kind === "file" &&
    (activeTab.language === "markdown" || isHtmlPreviewable(activeTab.path))
  const canOpenInBrowser =
    activeTab?.kind === "file" && isHtmlPreviewable(activeTab.path)
  const isPreviewActive =
    canPreview && activeFileTabId
      ? previewFileTabIds.has(activeFileTabId)
      : false
  const canTranslate = activeTab != null && isTranslationEligible(activeTab)
  const canSaveTranslation =
    activeTab != null &&
    activeTab.kind === "file" &&
    activeTab.transient?.type === "translation"

  const handleTranslate = useCallback(async () => {
    if (!activeTab || !isTranslationEligible(activeTab)) return
    // Double-click / re-entry guard before any async work.
    if (translateBusyRef.current) return
    if (!autoTitleAgent) {
      toast.error(t("translateAgentNotConfigured"))
      return
    }

    // Snapshot at click — later editor edits must not change the payload.
    const snapshotContent = activeTab.content
    const snapshotPath = activeTab.path
    const snapshotTitle = activeTab.title
    const snapshotId = activeTab.id
    const requestGen = beginTranslateRequest(snapshotId)
    const locale = intlLocaleToWire(intlLocale)
    const format = formatFromTranslatablePath(snapshotPath ?? snapshotTitle)
    const sourceContentHash = hashDocumentContent(snapshotContent)

    translateBusyRef.current = true
    setTranslateBusy(true)
    try {
      const result = await translateDocument({
        content: snapshotContent,
        format,
        locale,
        displayName: snapshotTitle,
      })
      openTranslationResultTab({
        sourceTabId: snapshotId,
        requestGen,
        content: result.translatedContent,
        locale: result.locale,
        format: result.format,
        sourcePath: snapshotPath,
        sourceContentHash,
        sourceTitle: snapshotTitle,
      })
    } catch (error) {
      const appError = extractAppCommandError(error)
      const message = appError?.i18n_key
        ? toLocalizedErrorMessage(error, tRoot as unknown as AppErrorTranslator)
        : t("translateFailed")
      toast.error(message)
    } finally {
      translateBusyRef.current = false
      setTranslateBusy(false)
    }
  }, [
    activeTab,
    autoTitleAgent,
    beginTranslateRequest,
    intlLocale,
    openTranslationResultTab,
    t,
    tRoot,
  ])

  const handleSaveTranslationAs = useCallback(async () => {
    if (!activeTab || activeTab.transient?.type !== "translation") return
    if (saveAsBusyRef.current) return
    if (activeFolder?.id == null) {
      toast.error(t("translateSavePathRejected"))
      return
    }

    const suggested =
      activeTab.transient.suggestedName || activeTab.title || "translation.md"
    const entered =
      typeof window !== "undefined"
        ? window.prompt(t("saveTranslationAs"), suggested)
        : null
    if (entered == null) return
    const relativePath = entered.trim()
    if (!relativePath) {
      toast.error(t("translateSavePathRejected"))
      return
    }

    const transientTabId = activeTab.id
    const content = activeTab.content

    saveAsBusyRef.current = true
    setSaveAsBusy(true)
    try {
      const saved = await saveTranslationAs({
        folderId: activeFolder.id,
        relativePath,
        content,
      })
      const settle = await openFilePreview(saved.absolutePath, {
        maximizeOnSuccess: false,
      })
      // Close the transient tab only after the real path loaded successfully.
      if (settle.ok) {
        closeFileTab(transientTabId)
      }
    } catch (error) {
      const appError = extractAppCommandError(error)
      const message = appError?.i18n_key
        ? toLocalizedErrorMessage(error, tRoot as unknown as AppErrorTranslator)
        : t("translateFailed")
      toast.error(message)
    } finally {
      saveAsBusyRef.current = false
      setSaveAsBusy(false)
    }
  }, [activeFolder?.id, activeTab, closeFileTab, openFilePreview, t, tRoot])

  if (fileTabs.length === 0) {
    return (
      <div className="h-10 px-3 flex items-center border-b border-border text-xs text-muted-foreground">
        {t("files")}
      </div>
    )
  }

  return (
    <div className="flex items-stretch">
      <Reorder.Group
        as="div"
        ref={scrollRef}
        role="tablist"
        axis="x"
        values={fileTabs}
        onReorder={handleReorder}
        onWheel={handleWheel}
        onMouseEnter={() => setIsHovered(true)}
        onMouseLeave={() => setIsHovered(false)}
        className={cn(
          "h-10 pt-1.5 px-1.5 flex-1 min-w-0 flex items-stretch gap-1.5 border-b border-border",
          "overflow-x-scroll",
          isHovered
            ? [
                "pb-0.5",
                "[&::-webkit-scrollbar]:h-1",
                "[&::-webkit-scrollbar-track]:bg-transparent",
                "[&::-webkit-scrollbar-thumb]:rounded-full",
                "[&::-webkit-scrollbar-thumb]:bg-border",
              ]
            : ["pb-1.5", "[&::-webkit-scrollbar]:h-0"]
        )}
      >
        {fileTabs.map((tab) => (
          <FileWorkspaceTabItem
            key={tab.id}
            tab={tab}
            active={tab.id === activeFileTabId}
            closeLabel={t("closeFileTab")}
            closeText={t("close")}
            closeOthersText={t("closeOthers")}
            closeAllText={t("closeAll")}
            isCoarsePointer={isCoarsePointer}
            isTouchSorting={touchSortingTabId === tab.id}
            onSwitch={switchFileTab}
            onClose={closeFileTab}
            onCloseOthers={closeOtherFileTabs}
            onCloseAll={closeAllFileTabs}
            onTouchSortingStart={setTouchSortingTabId}
            onTouchSortingEnd={handleTouchSortingEnd}
          />
        ))}
      </Reorder.Group>
      {canPreview && activeFileTabId && (
        <button
          type="button"
          onClick={() => toggleFileTabPreview(activeFileTabId)}
          className={cn(
            "shrink-0 flex items-center justify-center w-10 border-b border-border hover:bg-primary/8 transition-colors",
            isPreviewActive && "text-primary"
          )}
          aria-label={isPreviewActive ? t("editSource") : t("preview")}
          title={isPreviewActive ? t("editSource") : t("preview")}
        >
          {isPreviewActive ? (
            <Code className="h-4 w-4" />
          ) : (
            <Eye className="h-4 w-4" />
          )}
        </button>
      )}
      {canOpenInBrowser && activeTab?.path && (
        <button
          type="button"
          onClick={() => {
            // File tab paths are absolute — hand the path straight to the OS.
            openPath(activeTab.path as string).catch(() => {})
          }}
          className="shrink-0 flex items-center justify-center w-10 border-b border-border hover:bg-primary/8 transition-colors"
          aria-label={t("preview")}
          title={t("preview")}
        >
          <ExternalLink className="h-4 w-4" />
        </button>
      )}
      {canTranslate && (
        <button
          type="button"
          onClick={() => {
            void handleTranslate()
          }}
          disabled={translateBusy}
          className={cn(
            "shrink-0 flex items-center justify-center w-10 border-b border-border hover:bg-primary/8 transition-colors",
            "disabled:opacity-50 disabled:pointer-events-none"
          )}
          aria-label={
            translateBusy ? t("translating") : t("translateToCurrentLanguage")
          }
          title={
            translateBusy ? t("translating") : t("translateToCurrentLanguage")
          }
          aria-busy={translateBusy}
          data-testid="translate-document"
        >
          {translateBusy ? (
            <Loader2 className="h-4 w-4 animate-spin" />
          ) : (
            <Languages className="h-4 w-4" />
          )}
        </button>
      )}
      {canSaveTranslation && (
        <button
          type="button"
          onClick={() => {
            void handleSaveTranslationAs()
          }}
          disabled={saveAsBusy}
          className={cn(
            "shrink-0 flex items-center justify-center w-10 border-b border-border hover:bg-primary/8 transition-colors",
            "disabled:opacity-50 disabled:pointer-events-none"
          )}
          aria-label={t("saveTranslationAs")}
          title={t("saveTranslationAs")}
          aria-busy={saveAsBusy}
          data-testid="save-translation-as"
        >
          {saveAsBusy ? (
            <Loader2 className="h-4 w-4 animate-spin" />
          ) : (
            <Save className="h-4 w-4" />
          )}
        </button>
      )}
      {!isMobile && mode === "fusion" && (
        <button
          type="button"
          onClick={toggleFilesMaximized}
          className={cn(
            "shrink-0 flex items-center justify-center w-10 border-b border-border hover:bg-primary/8 transition-colors",
            filesMaximized && "text-primary"
          )}
          aria-label={filesMaximized ? t("restore") : t("maximize")}
          aria-pressed={filesMaximized}
          title={filesMaximized ? t("restore") : t("maximize")}
        >
          {filesMaximized ? (
            <Minimize2 className="h-4 w-4" />
          ) : (
            <Maximize2 className="h-4 w-4" />
          )}
        </button>
      )}
    </div>
  )
}

interface FileWorkspaceTabItemProps {
  tab: FileWorkspaceTab
  active: boolean
  closeLabel: string
  closeText: string
  closeOthersText: string
  closeAllText: string
  isCoarsePointer: boolean
  isTouchSorting: boolean
  onSwitch: (tabId: string) => void
  onClose: (tabId: string) => void
  onCloseOthers: (tabId: string) => void
  onCloseAll: () => void
  onTouchSortingStart: (tabId: string) => void
  onTouchSortingEnd: () => void
}

const FileWorkspaceTabItem = memo(function FileWorkspaceTabItem({
  tab,
  active,
  closeLabel,
  closeText,
  closeOthersText,
  closeAllText,
  isCoarsePointer,
  isTouchSorting,
  onSwitch,
  onClose,
  onCloseOthers,
  onCloseAll,
  onTouchSortingStart,
  onTouchSortingEnd,
}: FileWorkspaceTabItemProps) {
  const isDiff = tab.kind === "diff" || tab.kind === "rich-diff"
  const isDirty = tab.kind === "file" && Boolean(tab.isDirty)

  const handleLongPressStart = useCallback(
    () => onTouchSortingStart(tab.id),
    [onTouchSortingStart, tab.id]
  )

  const { dragControls, gestureHandlers } = useLongPressDrag({
    enabled: isCoarsePointer,
    onStart: handleLongPressStart,
    onEnd: onTouchSortingEnd,
  })

  const handleSwitch = useCallback(() => {
    onSwitch(tab.id)
  }, [onSwitch, tab.id])

  const whileDrag = useMemo(() => ({ scale: 1.03 }), [])

  return (
    <Reorder.Item
      as="div"
      value={tab}
      data-file-tab-id={tab.id}
      drag="x"
      dragControls={dragControls}
      dragListener={!isCoarsePointer}
      whileDrag={whileDrag}
      {...gestureHandlers}
      className={cn(
        "shrink-0 rounded-full cursor-grab active:cursor-grabbing",
        isTouchSorting && "z-50 opacity-90 shadow-md ring-1 ring-primary/25"
      )}
    >
      <ContextMenu>
        <ContextMenuTrigger asChild disabled={isTouchSorting}>
          <div
            role="tab"
            aria-selected={active}
            onClick={handleSwitch}
            onMouseDown={(event) =>
              handleMiddleClickClose(event, () => onClose(tab.id))
            }
            className={cn(
              "group/filetab relative flex items-center h-full gap-1.5 px-3 text-xs rounded-full",
              "cursor-pointer select-none shrink-0 hover:bg-primary/8 transition-colors",
              active ? "bg-primary/10 text-foreground" : "text-muted-foreground"
            )}
            title={tab.description ?? tab.title}
          >
            {isDiff ? (
              <GitCompare className="h-3.5 w-3.5" />
            ) : (
              <FileText className="h-3.5 w-3.5" />
            )}
            <span className="truncate max-w-[180px]">
              {tab.title}
              {isDirty ? " *" : ""}
            </span>
            <button
              type="button"
              className={cn(
                "rounded-full p-0.5 hover:bg-muted",
                active
                  ? "opacity-100"
                  : "opacity-0 group-hover/filetab:opacity-100"
              )}
              onClick={(event) => {
                event.stopPropagation()
                onClose(tab.id)
              }}
              aria-label={closeLabel}
            >
              <X className="h-3 w-3" />
            </button>
          </div>
        </ContextMenuTrigger>
        <ContextMenuContent>
          <ContextMenuItem onSelect={() => onClose(tab.id)}>
            {closeText}
          </ContextMenuItem>
          <ContextMenuItem onSelect={() => onCloseOthers(tab.id)}>
            {closeOthersText}
          </ContextMenuItem>
          <ContextMenuSeparator />
          <ContextMenuItem onSelect={onCloseAll}>
            {closeAllText}
          </ContextMenuItem>
        </ContextMenuContent>
      </ContextMenu>
    </Reorder.Item>
  )
})
