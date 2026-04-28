"use client"

import { useEffect, useMemo, useState } from "react"
import { Loader2, Server, Settings2 } from "lucide-react"
import { useTranslations } from "next-intl"
import { toast } from "sonner"

import { listConnections, openSettingsWindow } from "@/lib/api"
import { toErrorMessage } from "@/lib/app-error"
import { useAppWorkspace } from "@/contexts/app-workspace-context"
import type { ConnectionConfig } from "@/lib/types"

import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"

interface SshFolderDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

export function SshFolderDialog({ open, onOpenChange }: SshFolderDialogProps) {
  const t = useTranslations("SshFolderDialog")
  const { openRemoteFolder } = useAppWorkspace()

  const [connections, setConnections] = useState<ConnectionConfig[] | null>(
    null
  )
  const [loading, setLoading] = useState(false)
  const [selectedId, setSelectedId] = useState<string>("")
  const [remotePath, setRemotePath] = useState("")
  const [submitting, setSubmitting] = useState(false)
  const [pathError, setPathError] = useState<string | null>(null)

  // Load connections every time the dialog opens — the list may have
  // changed since the last render (CRUD happens in another window).
  useEffect(() => {
    if (!open) return
    let cancelled = false
    setLoading(true)
    listConnections()
      .then((rows) => {
        if (cancelled) return
        const ssh = rows.filter((r) => r.kind === "ssh")
        setConnections(ssh)
        // Preselect the first connection if nothing is chosen yet.
        if (ssh.length > 0 && !selectedId) {
          setSelectedId(ssh[0].id)
        }
      })
      .catch((err) => {
        if (cancelled) return
        toast.error(toErrorMessage(err))
        setConnections([])
      })
      .finally(() => {
        if (!cancelled) setLoading(false)
      })
    return () => {
      cancelled = true
    }
    // selectedId intentionally excluded: refetching when the user picks an
    // entry would reload + clobber their choice.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open])

  // Reset path / error when the dialog closes so a future open starts clean.
  useEffect(() => {
    if (!open) {
      setRemotePath("")
      setPathError(null)
      setSubmitting(false)
    }
  }, [open])

  const selected = useMemo(
    () => connections?.find((c) => c.id === selectedId) ?? null,
    [connections, selectedId]
  )

  function validate(p: string): string | null {
    const trimmed = p.trim()
    if (trimmed === "") return t("errPathRequired")
    if (!trimmed.startsWith("/") && !trimmed.startsWith("~")) {
      return t("errPathRelative")
    }
    return null
  }

  async function handleOpen() {
    const err = validate(remotePath)
    if (err) {
      setPathError(err)
      return
    }
    if (!selected) return
    setSubmitting(true)
    try {
      await openRemoteFolder(selected.id, remotePath.trim())
      toast.success(t("openSuccess"))
      onOpenChange(false)
    } catch (e) {
      toast.error(`${t("openFailed")} ${toErrorMessage(e)}`)
    } finally {
      setSubmitting(false)
    }
  }

  function describeConnection(c: ConnectionConfig): string {
    if (c.ssh_alias) return c.ssh_alias
    const user = c.ssh_user ?? ""
    const host = c.ssh_host ?? ""
    return user ? `${user}@${host}` : host
  }

  const empty = connections !== null && connections.length === 0
  const canOpen =
    !!selected && !submitting && remotePath.trim() !== "" && !pathError

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Server className="size-4" />
            {t("title")}
          </DialogTitle>
          <DialogDescription>{t("description")}</DialogDescription>
        </DialogHeader>

        {loading ? (
          <div className="flex items-center justify-center py-8 text-sm text-muted-foreground">
            <Loader2 className="mr-2 size-4 animate-spin" />
            {t("loading")}
          </div>
        ) : empty ? (
          <div className="space-y-3 py-4 text-sm text-muted-foreground">
            <p>{t("noConnections")}</p>
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                onOpenChange(false)
                openSettingsWindow("ssh-connections").catch((err) =>
                  console.error(
                    "[SshFolderDialog] failed to open settings:",
                    err
                  )
                )
              }}
            >
              <Settings2 className="mr-1 size-3.5" />
              {t("manageConnections")}
            </Button>
          </div>
        ) : (
          <div className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="ssh-folder-connection">{t("connection")}</Label>
              <Select value={selectedId} onValueChange={setSelectedId}>
                <SelectTrigger id="ssh-folder-connection">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {connections?.map((c) => (
                    <SelectItem key={c.id} value={c.id}>
                      <div className="flex flex-col">
                        <span>{c.name}</span>
                        <span className="text-xs text-muted-foreground">
                          {describeConnection(c)}
                        </span>
                      </div>
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>

            <div className="space-y-2">
              <Label htmlFor="ssh-folder-path">{t("remotePath")}</Label>
              <Input
                id="ssh-folder-path"
                placeholder={t("remotePathPlaceholder")}
                value={remotePath}
                onChange={(e) => {
                  setRemotePath(e.target.value)
                  if (pathError) setPathError(validate(e.target.value))
                }}
                onBlur={() => {
                  if (remotePath !== "") setPathError(validate(remotePath))
                }}
                disabled={submitting}
                autoFocus
              />
              {pathError ? (
                <p className="text-xs text-destructive">{pathError}</p>
              ) : (
                <p className="text-xs text-muted-foreground">
                  {t("remotePathHint")}
                </p>
              )}
            </div>
          </div>
        )}

        <DialogFooter>
          {!empty && (
            <Button
              variant="ghost"
              onClick={() => {
                onOpenChange(false)
                openSettingsWindow("ssh-connections").catch((err) =>
                  console.error(
                    "[SshFolderDialog] failed to open settings:",
                    err
                  )
                )
              }}
              disabled={submitting}
            >
              {t("manageConnections")}
            </Button>
          )}
          <Button
            variant="outline"
            onClick={() => onOpenChange(false)}
            disabled={submitting}
          >
            {t("cancel")}
          </Button>
          {!empty && (
            <Button onClick={handleOpen} disabled={!canOpen}>
              {submitting && <Loader2 className="mr-1 size-3.5 animate-spin" />}
              {t("open")}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
