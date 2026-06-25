"use client"

import { useEffect, useMemo, useState } from "react"
import { useTranslations } from "next-intl"
import { FileWarning, Sparkles } from "lucide-react"

import { officecliRenderHtml, openSettingsWindow } from "@/lib/api"
import { withSandboxCsp } from "@/lib/html-preview-inline"
import { useWorkspaceStateStore } from "@/hooks/use-workspace-state-store"
import type { FileWorkspaceTab } from "@/contexts/workspace-context"
import { cn } from "@/lib/utils"

function normalizeComparePath(path: string): string {
  return path.replace(/\\/g, "/").replace(/\/+$/, "")
}

// Trusted sandbox: scripts run, popups/forms/modals work, but the frame still
// has an opaque origin and cannot navigate the top window. Only used for the
// pptx "full render" opt-in (Morph/3D/KaTeX via CDN). docx/xlsx never need it.
const SANDBOX_TRUSTED = "allow-scripts allow-popups allow-forms allow-modals"

function isPptxPath(path: string): boolean {
  return path.toLowerCase().endsWith(".pptx")
}

/**
 * Preview a .docx/.xlsx/.pptx file by rendering it to self-contained HTML with
 * the OfficeCLI backend (`officecli view <file> html`) and showing it in a
 * sandboxed iframe — the same security boundary as {@link HtmlPreview}.
 *
 * docx/xlsx render as pure static HTML (no scripts). pptx slide content is
 * static too, but Morph animations / 3D / math need the slide's own scripts and
 * a CDN; pptx therefore opens in the full (trusted-sandbox) render by default,
 * and the "full render" toggle drops back to the safe, offline static slides.
 *
 * Live preview: the component subscribes to the workspace file-watch stream and
 * re-renders whenever its own file changes on disk, so an open preview tracks
 * the agent's edits in real time without any manual refresh.
 */
export function OfficePreview({
  tab,
  folderPath,
}: {
  tab: FileWorkspaceTab
  folderPath: string | null
}) {
  const t = useTranslations("Folder.fileWorkspacePanel")
  const [html, setHtml] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [notInstalled, setNotInstalled] = useState(false)
  // Default pptx to the full (trusted) render so Morph/3D/math show on open;
  // the toggle drops back to the safe static slides. No effect for docx/xlsx,
  // which have no scripts (trusted = pptx && fullRender stays false there).
  const [fullRender, setFullRender] = useState(true)
  // Bumped by the file-watch subscription below to re-fetch on disk changes.
  const [refreshSeq, setRefreshSeq] = useState(0)

  const path = tab.path ?? ""
  const pptx = isPptxPath(path)
  const trusted = pptx && fullRender
  const heading = path.split("/").pop() || path

  // Re-render live as the underlying file changes. Uses the shared, per-root
  // workspace-state store (refcounted), so this adds no extra file watcher.
  const { subscribeEnvelopes } = useWorkspaceStateStore(folderPath)
  useEffect(() => {
    if (!folderPath || !path) return
    const target = normalizeComparePath(path)
    return subscribeEnvelopes(({ changed_paths }) => {
      if (!changed_paths || changed_paths.length === 0) return
      if (changed_paths.some((p) => normalizeComparePath(p) === target)) {
        setRefreshSeq((s) => s + 1)
      }
    })
  }, [subscribeEnvelopes, folderPath, path])

  useEffect(() => {
    let cancelled = false
    const root = folderPath ?? ""
    if (!root || !path) return
    // State is only set inside the async callbacks (never synchronously in the
    // effect body — the repo lints against that). A different file remounts the
    // component (keyed by tab id upstream), so there is no stale carry-over; a
    // refreshKey bump keeps the prior render visible until the new HTML lands
    // (no flash during live refresh).
    officecliRenderHtml(root, path)
      .then((rendered) => {
        if (cancelled) return
        setHtml(rendered)
        setError(null)
        setNotInstalled(false)
      })
      .catch((err) => {
        if (cancelled) return
        const msg = String(err)
        if (/not installed/i.test(msg)) {
          setNotInstalled(true)
        } else {
          setError(msg)
        }
      })
    return () => {
      cancelled = true
    }
  }, [path, folderPath, refreshSeq])

  const srcDoc = useMemo(
    () => (html != null ? withSandboxCsp(html, { trusted }) : ""),
    [html, trusted]
  )
  const loading = html == null && error == null && !notInstalled

  if (notInstalled) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-3 px-6 text-center">
        <FileWarning className="h-8 w-8 text-muted-foreground" />
        <div className="text-sm font-medium text-foreground">
          {t("officeNotInstalled")}
        </div>
        <div className="max-w-sm text-xs text-muted-foreground">
          {t("officeNotInstalledHint")}
        </div>
        <button
          type="button"
          onClick={() => {
            openSettingsWindow().catch(() => {})
          }}
          className="mt-1 rounded-md border border-border bg-card px-3 py-1.5 text-xs font-medium text-foreground transition-colors hover:bg-primary/8"
        >
          {t("officeOpenSettings")}
        </button>
      </div>
    )
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex h-9 shrink-0 items-center justify-between gap-3 border-b border-border bg-muted/20 px-3">
        <span
          className="min-w-0 truncate text-xs font-medium text-foreground/80"
          title={heading || undefined}
        >
          {heading}
        </span>
        {pptx && (
          <button
            type="button"
            onClick={() => setFullRender((v) => !v)}
            aria-pressed={fullRender}
            title={t("officeFullRenderHint")}
            className={cn(
              "inline-flex shrink-0 items-center gap-1.5 rounded-md px-2 py-1 text-xs transition-colors",
              fullRender
                ? "text-amber-600 hover:bg-amber-500/10 dark:text-amber-500"
                : "text-muted-foreground hover:bg-primary/8"
            )}
          >
            <Sparkles className="h-3.5 w-3.5" />
            {t("officeFullRender")}
          </button>
        )}
      </div>
      <div className="relative min-h-0 flex-1">
        {loading && (
          <div className="absolute right-3 top-2 z-10 rounded-md bg-background/70 px-2 py-1 text-[11px] text-muted-foreground backdrop-blur-sm">
            {t("loading")}
          </div>
        )}
        {error ? (
          <div className="flex h-full items-center justify-center px-6 text-center text-xs text-muted-foreground">
            {error}
          </div>
        ) : (
          html != null && (
            <iframe
              key={trusted ? "trusted" : "strict"}
              title={t("officePreviewTitle")}
              sandbox={trusted ? SANDBOX_TRUSTED : ""}
              srcDoc={srcDoc}
              className="absolute inset-0 h-full w-full border-0 bg-white"
            />
          )
        )}
      </div>
    </div>
  )
}
