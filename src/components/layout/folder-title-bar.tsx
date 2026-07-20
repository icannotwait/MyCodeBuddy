"use client"

import { useCallback } from "react"
import {
  EllipsisVertical,
  Menu,
  PanelRight,
  Settings,
  SquareTerminal,
} from "lucide-react"
import { useTranslations } from "next-intl"
import { openSettingsWindow } from "@/lib/api"
import { useActiveFolder } from "@/contexts/active-folder-context"
import { useIsActiveChatMode } from "@/hooks/use-is-active-chat-mode"
import { Button } from "@/components/ui/button"
import { useSidebarContext } from "@/contexts/sidebar-context"
import { useAuxPanelContext } from "@/contexts/aux-panel-context"
import { useTerminalContext } from "@/contexts/terminal-context"
import { AppTitleBar } from "./app-title-bar"
import { NewFolderDropdown } from "./new-folder-dropdown"
import { RemoteWorkspaceDropdown } from "./remote-workspace-dropdown"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"

/**
 * Mobile-only workspace title bar.
 *
 * On desktop the full-width title bar was removed: its buttons were relocated
 * into per-column edge clusters (`LeftEdgeChrome` / `RightEdgeChrome`) so the
 * four columns' divider lines run unbroken from the top, and its global
 * shortcuts + search/directory dialogs moved to `WorkspaceChromeController`.
 * This component is mounted only on the mobile path (`FolderLayoutShell`), where
 * the sidebar / aux / terminal are `Sheet` overlays that still need a compact
 * bar to summon them.
 */
export function FolderTitleBar() {
  const tTitleBar = useTranslations("Folder.folderTitleBar")
  const { toggle } = useSidebarContext()
  const { toggle: toggleAuxPanel } = useAuxPanelContext()
  const { toggle: toggleTerminal } = useTerminalContext()
  const { activeFolder } = useActiveFolder()
  const isChatMode = useIsActiveChatMode()

  const handleOpenSettings = useCallback(() => {
    openSettingsWindow().catch((err) => {
      console.error("[FolderTitleBar] failed to open settings:", err)
    })
  }, [])

  return (
    <AppTitleBar
      left={
        <div className="flex min-w-0 items-center gap-2">
          <Button
            variant="ghost"
            size="icon"
            className="h-8 w-8 shrink-0"
            onClick={toggle}
          >
            <Menu className="h-4 w-4" />
          </Button>
          <NewFolderDropdown />
          <RemoteWorkspaceDropdown />
        </div>
      }
      right={
        <div className="flex items-center gap-1">
          {/* Search lives in the left sidebar's fixed actions region; the ⌘K
              shortcut + SearchCommandDialog live in WorkspaceChromeController. */}
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="ghost" size="icon" className="h-8 w-8">
                <EllipsisVertical className="h-4 w-4" />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              {/* The aux panel hosts the Session Details tab, so it's reachable
                  in chat mode too. */}
              <DropdownMenuItem
                onClick={toggleAuxPanel}
                disabled={!activeFolder && !isChatMode}
              >
                <PanelRight className="h-3.5 w-3.5" />
                {tTitleBar("toggleAuxPanel")}
              </DropdownMenuItem>
              <DropdownMenuItem
                onClick={() => toggleTerminal()}
                disabled={!activeFolder}
              >
                <SquareTerminal className="h-3.5 w-3.5" />
                {tTitleBar("toggleTerminal")}
              </DropdownMenuItem>
              <DropdownMenuItem onClick={handleOpenSettings}>
                <Settings className="h-3.5 w-3.5" />
                {tTitleBar("openSettings")}
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      }
    />
  )
}
