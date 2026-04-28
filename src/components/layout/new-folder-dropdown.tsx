"use client"

import { useState } from "react"
import { FolderOpen, FolderPlus, GitBranch, Rocket, Server } from "lucide-react"
import { useTranslations } from "next-intl"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { Button } from "@/components/ui/button"
import { openProjectBootWindow } from "@/lib/api"
import { isDesktop, openFileDialog } from "@/lib/platform"
import { useAppWorkspace } from "@/contexts/app-workspace-context"
import { CloneDialog } from "@/components/layout/clone-dialog"
import { DirectoryBrowserDialog } from "@/components/shared/directory-browser-dialog"
import { SshFolderDialog } from "@/components/layout/ssh-folder-dialog"

export function NewFolderDropdown() {
  const t = useTranslations("Folder.folderNameDropdown")
  const { openFolder } = useAppWorkspace()
  const [cloneOpen, setCloneOpen] = useState(false)
  const [browserOpen, setBrowserOpen] = useState(false)
  const [sshOpen, setSshOpen] = useState(false)

  async function handleOpenFolder() {
    if (isDesktop()) {
      const selected = await openFileDialog({
        directory: true,
        multiple: false,
      })
      if (selected) {
        await openFolder(Array.isArray(selected) ? selected[0] : selected)
      }
    } else {
      setBrowserOpen(true)
    }
  }

  return (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            variant="ghost"
            size="icon"
            className="h-6 w-6 hover:text-foreground/80"
            title={t("openFolder")}
          >
            <FolderPlus className="h-3.5 w-3.5" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent className="min-w-56" align="start">
          <DropdownMenuItem onSelect={handleOpenFolder}>
            <FolderOpen className="h-3.5 w-3.5 shrink-0" />
            {t("openFolder")}
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={() => setCloneOpen(true)}>
            <GitBranch className="h-3.5 w-3.5 shrink-0" />
            {t("cloneRepository")}
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={() => openProjectBootWindow()}>
            <Rocket className="h-3.5 w-3.5 shrink-0" />
            {t("projectBoot")}
          </DropdownMenuItem>
          {isDesktop() && (
            <DropdownMenuItem onSelect={() => setSshOpen(true)}>
              <Server className="h-3.5 w-3.5 shrink-0" />
              {t("openFromSsh")}
            </DropdownMenuItem>
          )}
        </DropdownMenuContent>
      </DropdownMenu>
      <CloneDialog open={cloneOpen} onOpenChange={setCloneOpen} />
      <SshFolderDialog open={sshOpen} onOpenChange={setSshOpen} />
      <DirectoryBrowserDialog
        open={browserOpen}
        onOpenChange={setBrowserOpen}
        onSelect={(path) => {
          openFolder(path).catch((err) => {
            console.error("[NewFolderDropdown] failed to open folder:", err)
          })
        }}
      />
    </>
  )
}
