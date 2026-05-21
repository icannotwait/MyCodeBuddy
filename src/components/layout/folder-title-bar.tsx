"use client"

import { useCallback, useEffect, useState } from "react"
import {
  EllipsisVertical,
  Menu,
  PanelLeft,
  PanelRight,
  PawPrint,
  Search,
  Settings,
  SquareTerminal,
} from "lucide-react"
import { useTranslations } from "next-intl"
import { openSettingsWindow } from "@/lib/api"
import { getPetSettings, openPetWindow } from "@/lib/pet/api"
import { useAppWorkspace } from "@/contexts/app-workspace-context"
import { useActiveFolder } from "@/contexts/active-folder-context"
import { isDesktop, openFileDialog } from "@/lib/platform"
import { getActiveRemoteConnectionId } from "@/lib/transport"
import { Button } from "@/components/ui/button"
import { useSidebarContext } from "@/contexts/sidebar-context"
import { useAuxPanelContext } from "@/contexts/aux-panel-context"
import { useTerminalContext } from "@/contexts/terminal-context"
import { useTabContext } from "@/contexts/tab-context"
import { useIsMac } from "@/hooks/use-is-mac"
import { useShortcutSettings } from "@/hooks/use-shortcut-settings"
import {
  formatShortcutLabel,
  matchShortcutEvent,
} from "@/lib/keyboard-shortcuts"
import { AppTitleBar } from "./app-title-bar"
import { BranchDropdown } from "./branch-dropdown"
import { CommandDropdown } from "./command-dropdown"
import { NewFolderDropdown } from "./new-folder-dropdown"
import { RemoteWorkspaceDropdown } from "./remote-workspace-dropdown"
import { SearchCommandDialog } from "@/components/conversations/search-command-dialog"
import { DirectoryBrowserDialog } from "@/components/shared/directory-browser-dialog"
import { useIsMobile } from "@/hooks/use-mobile"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"

export function FolderTitleBar() {
  const tTitleBar = useTranslations("Folder.folderTitleBar")
  const tPet = useTranslations("Pet")
  const { openFolder } = useAppWorkspace()
  const { activeFolder } = useActiveFolder()
  const { isOpen, toggle } = useSidebarContext()
  const { isOpen: auxPanelOpen, toggle: toggleAuxPanel } = useAuxPanelContext()
  const { isOpen: terminalOpen, toggle: toggleTerminal } = useTerminalContext()
  const { openNewConversationTab } = useTabContext()
  const isMac = useIsMac()
  const { shortcuts } = useShortcutSettings()
  const [searchOpen, setSearchOpen] = useState(false)
  const [browserOpen, setBrowserOpen] = useState(false)

  const handleOpenPet = useCallback(async () => {
    if (!isDesktop()) return
    try {
      const settings = await getPetSettings()
      if (!settings.activePetId) {
        await openSettingsWindow("appearance")
        return
      }
      await openPetWindow()
    } catch {
      // No active pet or window error — route the user to the manager.
      try {
        await openSettingsWindow("appearance")
      } catch (err) {
        console.warn("[Pet] open settings failed:", err)
      }
    }
  }, [])

  const handleOpenFolder = useCallback(async () => {
    // See NewFolderDropdown / SidebarConversationList for the same logic:
    // the native Tauri dialog browses the LOCAL filesystem, so when the
    // user is bound to a remote workspace we must fall through to the
    // in-app DirectoryBrowserDialog (which browses the remote host via
    // the proxied `list_directory_entries`).
    if (isDesktop() && getActiveRemoteConnectionId() === null) {
      try {
        const result = await openFileDialog({
          directory: true,
          multiple: false,
        })
        if (!result) return
        const selected = Array.isArray(result) ? result[0] : result
        await openFolder(selected)
      } catch (err) {
        console.error("[FolderTitleBar] failed to open folder:", err)
      }
    } else {
      setBrowserOpen(true)
    }
  }, [openFolder])

  const handleOpenSettings = useCallback(() => {
    openSettingsWindow().catch((err) => {
      console.error("[FolderTitleBar] failed to open settings:", err)
    })
  }, [])

  useEffect(() => {
    function handleKeyDown(e: KeyboardEvent) {
      if (matchShortcutEvent(e, shortcuts.toggle_search)) {
        e.preventDefault()
        setSearchOpen((prev) => !prev)
        return
      }
      if (matchShortcutEvent(e, shortcuts.toggle_sidebar)) {
        e.preventDefault()
        toggle()
        return
      }
      if (matchShortcutEvent(e, shortcuts.toggle_terminal)) {
        e.preventDefault()
        toggleTerminal()
        return
      }
      if (matchShortcutEvent(e, shortcuts.toggle_aux_panel)) {
        e.preventDefault()
        toggleAuxPanel()
        return
      }
      if (matchShortcutEvent(e, shortcuts.new_conversation)) {
        if (!activeFolder) return
        e.preventDefault()
        openNewConversationTab(activeFolder.id, activeFolder.path)
        return
      }
      if (matchShortcutEvent(e, shortcuts.open_folder)) {
        e.preventDefault()
        void handleOpenFolder()
        return
      }
      if (matchShortcutEvent(e, shortcuts.open_settings)) {
        e.preventDefault()
        handleOpenSettings()
      }
    }
    document.addEventListener("keydown", handleKeyDown)
    return () => document.removeEventListener("keydown", handleKeyDown)
  }, [
    activeFolder,
    handleOpenFolder,
    handleOpenSettings,
    openNewConversationTab,
    shortcuts,
    toggle,
    toggleAuxPanel,
    toggleTerminal,
  ])

  const isMobile = useIsMobile()
  return (
    <>
      <AppTitleBar
        left={
          isMobile ? (
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
              <BranchDropdown />
            </div>
          ) : (
            <div className="flex h-8 flex-1 items-center gap-6">
              <div className="flex items-center gap-2">
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-6 w-6 hover:text-foreground/80"
                  onClick={toggle}
                  title={tTitleBar("withShortcut", {
                    label: tTitleBar(isOpen ? "hideSidebar" : "showSidebar"),
                    shortcut: formatShortcutLabel(
                      shortcuts.toggle_sidebar,
                      isMac
                    ),
                  })}
                >
                  <PanelLeft className="h-3.5 w-3.5" />
                </Button>
                <NewFolderDropdown />
                <RemoteWorkspaceDropdown />
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-6 w-6 hover:text-foreground/80"
                  onClick={handleOpenPet}
                  title={tPet("manager.summon")}
                >
                  <PawPrint className="h-3.5 w-3.5" />
                </Button>
              </div>
              <BranchDropdown />
              <div data-tauri-drag-region className="h-8 flex-1" />
            </div>
          )
        }
        right={
          isMobile ? (
            <div className="flex items-center gap-1">
              <CommandDropdown />
              <Button
                variant="ghost"
                size="icon"
                className="h-8 w-8"
                onClick={() => setSearchOpen(true)}
                title={tTitleBar("search")}
              >
                <Search className="h-4 w-4" />
              </Button>
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button variant="ghost" size="icon" className="h-8 w-8">
                    <EllipsisVertical className="h-4 w-4" />
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end">
                  <DropdownMenuItem
                    onClick={toggleAuxPanel}
                    disabled={!activeFolder}
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
          ) : (
            <div className="flex items-center gap-10">
              <div className="flex items-center gap-2">
                <CommandDropdown />
              </div>
              <div className="flex items-center gap-2">
                <Button
                  variant="ghost"
                  size="icon"
                  className={`h-6 w-6 hover:text-foreground/80 ${terminalOpen ? "bg-accent" : ""}`}
                  onClick={() => toggleTerminal()}
                  disabled={!activeFolder}
                  title={tTitleBar("withShortcut", {
                    label: tTitleBar("toggleTerminal"),
                    shortcut: formatShortcutLabel(
                      shortcuts.toggle_terminal,
                      isMac
                    ),
                  })}
                >
                  <SquareTerminal className="h-3.5 w-3.5" />
                </Button>
                <Button
                  variant="ghost"
                  size="icon"
                  className={`h-6 w-6 hover:text-foreground/80 ${auxPanelOpen ? "bg-accent" : ""}`}
                  onClick={toggleAuxPanel}
                  disabled={!activeFolder}
                  title={tTitleBar("withShortcut", {
                    label: tTitleBar("toggleAuxPanel"),
                    shortcut: formatShortcutLabel(
                      shortcuts.toggle_aux_panel,
                      isMac
                    ),
                  })}
                >
                  <PanelRight className="h-3.5 w-3.5" />
                </Button>
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-6 w-6 hover:text-foreground/80"
                  onClick={() => setSearchOpen(true)}
                  title={tTitleBar("withShortcut", {
                    label: tTitleBar("search"),
                    shortcut: formatShortcutLabel(
                      shortcuts.toggle_search,
                      isMac
                    ),
                  })}
                >
                  <Search className="h-3.5 w-3.5" />
                </Button>
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-6 w-6 hover:text-foreground/80"
                  onClick={handleOpenSettings}
                  title={tTitleBar("withShortcut", {
                    label: tTitleBar("openSettings"),
                    shortcut: formatShortcutLabel(
                      shortcuts.open_settings,
                      isMac
                    ),
                  })}
                >
                  <Settings className="h-3.5 w-3.5" />
                </Button>
              </div>
            </div>
          )
        }
      />
      <SearchCommandDialog open={searchOpen} onOpenChange={setSearchOpen} />
      <DirectoryBrowserDialog
        open={browserOpen}
        onOpenChange={setBrowserOpen}
        onSelect={(path) => {
          openFolder(path).catch((err) => {
            console.error("[FolderTitleBar] failed to open folder:", err)
          })
        }}
      />
    </>
  )
}
