"use client"

import { useEffect, useState } from "react"
import { useLocale, useTranslations } from "next-intl"
import { Check, Copy, Download, FolderDown, LoaderCircle } from "lucide-react"
import { Button } from "@/components/ui/button"
import {
  conclusionErrorReason,
  downloadMarkdown,
  exportRoundtableConclusion,
  saveRoundtableConclusion,
  type RoundtableConclusionExport,
  type RoundtableConclusionSaveMode,
} from "@/lib/roundtable/conclusion"
import { copyTextToClipboard } from "@/lib/utils"

type Busy = "save" | "download" | "copy" | null

/** Save / download / copy for a completed room's accepted conclusion. */
export function RoundtableConclusionActions({ roomId }: { roomId: string }) {
  const t = useTranslations("Roundtable")
  const locale = useLocale()
  const [busy, setBusy] = useState<Busy>(null)
  const [savedPath, setSavedPath] = useState<string | null>(null)
  const [conflict, setConflict] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [workspaceAvailable, setWorkspaceAvailable] = useState(true)

  useEffect(() => {
    let cancelled = false
    exportRoundtableConclusion(roomId, locale)
      .then((value) => {
        if (cancelled) return
        setWorkspaceAvailable(value.workspace_available)
        setSavedPath(value.saves.at(-1)?.relative_path ?? null)
      })
      .catch(() => {})
    return () => {
      cancelled = true
    }
  }, [roomId, locale])

  const exported = async (): Promise<RoundtableConclusionExport> =>
    exportRoundtableConclusion(roomId, locale)

  const fail = (cause: unknown) => {
    const reason = conclusionErrorReason(cause)
    setError(
      reason === "conclusion_unavailable"
        ? t("conclusionUnavailable")
        : reason === "workspace_path_refused"
          ? t("conclusionPathRefused")
          : t("conclusionExportFailed")
    )
  }

  const save = async (mode: RoundtableConclusionSaveMode) => {
    setBusy("save")
    setError(null)
    setNotice(null)
    try {
      const result = await saveRoundtableConclusion(roomId, locale, mode)
      setConflict(null)
      setSavedPath(result.relative_path)
      setNotice(t("conclusionSaved", { path: result.relative_path }))
    } catch (cause) {
      if (
        conclusionErrorReason(cause) === "already_exists" &&
        mode === "create"
      ) {
        const value = await exported().catch(() => null)
        setConflict(value?.relative_path ?? "")
      } else {
        fail(cause)
      }
    } finally {
      setBusy(null)
    }
  }

  const download = async () => {
    setBusy("download")
    setError(null)
    setNotice(null)
    try {
      const value = await exported()
      if (await downloadMarkdown(value.file_name, value.markdown))
        setNotice(t("conclusionDownloaded", { name: value.file_name }))
    } catch (cause) {
      fail(cause)
    } finally {
      setBusy(null)
    }
  }

  const copy = async () => {
    setBusy("copy")
    setError(null)
    setNotice(null)
    try {
      const value = await exported()
      if (await copyTextToClipboard(value.markdown))
        setNotice(t("conclusionCopied"))
      else setError(t("conclusionCopyFailed"))
    } catch (cause) {
      fail(cause)
    } finally {
      setBusy(null)
    }
  }

  const spinner = (kind: Busy) =>
    busy === kind ? (
      <LoaderCircle aria-hidden="true" className="size-3.5 animate-spin" />
    ) : null

  return (
    <div
      role="group"
      aria-label={t("conclusionActions")}
      data-testid="roundtable-conclusion-actions"
      className="mt-2 flex min-w-0 flex-col gap-2 border-t border-primary/20 pt-3"
    >
      <div className="flex flex-wrap items-center gap-2">
        <Button
          type="button"
          size="sm"
          disabled={busy !== null || !workspaceAvailable}
          onClick={() => void save("create")}
        >
          {spinner("save") ?? (
            <FolderDown aria-hidden="true" className="size-3.5" />
          )}
          {t("conclusionSave")}
        </Button>
        <Button
          type="button"
          size="sm"
          variant="outline"
          disabled={busy !== null}
          onClick={() => void download()}
        >
          {spinner("download") ?? (
            <Download aria-hidden="true" className="size-3.5" />
          )}
          {t("conclusionDownload")}
        </Button>
        <Button
          type="button"
          size="sm"
          variant="outline"
          disabled={busy !== null}
          onClick={() => void copy()}
        >
          {spinner("copy") ?? <Copy aria-hidden="true" className="size-3.5" />}
          {t("conclusionCopy")}
        </Button>
      </div>
      {conflict !== null ? (
        <div
          role="alertdialog"
          aria-label={t("conclusionExistsTitle")}
          className="flex flex-wrap items-center gap-2 rounded-lg border border-amber-500/40 bg-amber-500/10 p-2 text-xs"
        >
          <span className="min-w-0 flex-1 break-all">
            {t("conclusionExists", { path: conflict })}
          </span>
          <Button
            type="button"
            size="sm"
            variant="destructive"
            disabled={busy !== null}
            onClick={() => void save("overwrite")}
          >
            {t("conclusionOverwrite")}
          </Button>
          <Button
            type="button"
            size="sm"
            variant="outline"
            disabled={busy !== null}
            onClick={() => void save("save_as")}
          >
            {t("conclusionSaveAs")}
          </Button>
          <Button
            type="button"
            size="sm"
            variant="ghost"
            onClick={() => setConflict(null)}
          >
            {t("conclusionCancel")}
          </Button>
        </div>
      ) : null}
      {notice ? (
        <p
          role="status"
          className="flex items-center gap-1 text-xs text-muted-foreground"
        >
          <Check aria-hidden="true" className="size-3.5 text-emerald-600" />
          {notice}
        </p>
      ) : null}
      {savedPath && !notice ? (
        <p className="text-xs text-muted-foreground">
          {t("conclusionSavedAt")}{" "}
          <code className="break-all">{savedPath}</code>
        </p>
      ) : null}
      {!workspaceAvailable ? (
        <p className="text-xs text-muted-foreground">
          {t("conclusionNoWorkspace")}
        </p>
      ) : null}
      {error ? (
        <p role="alert" className="text-xs text-destructive">
          {error}
        </p>
      ) : null}
    </div>
  )
}
