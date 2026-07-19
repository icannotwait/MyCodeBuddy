"use client"

/**
 * Shared secondary / operational / expand chrome for Codeg delegation cards.
 *
 * Used by both the message-stream `DelegatedSubThread` card and the
 * `SubAgentOverlay` Codeg row so the two surfaces never disagree about
 * title-first secondary text, runtime segments, attention, or file expand.
 *
 * Operational line is built by joining **present** localized segments with
 * `" | "` — never a single template that forces empty slots. Tool count and
 * elapsed reuse `Folder.chat.liveTurnStats` keys (no duplicated tool-count
 * strings). Expand control only appears when `touched_files.length > 0`.
 */

import { useId, useMemo } from "react"
import {
  Bell,
  ChevronDown,
  ChevronRight,
  FilePenLine,
  Timer,
  Wrench,
} from "lucide-react"
import { useTranslations } from "next-intl"

import { Badge } from "@/components/ui/badge"
import { formatElapsedLabel } from "@/lib/format-elapsed"
import type { EditRollupViewModel } from "@/lib/delegation-card"
import type {
  AttentionRequestSummary,
  DelegationRuntimeStats,
} from "@/lib/types"
import { cn } from "@/lib/utils"

export interface DelegationCardChromeProps {
  displaySecondary: string | null
  /** Prefer full title for secondary tooltip; fall back to task. */
  conversationTitle?: string | null
  task?: string | null
  elapsedMs: number | null
  /** null when stats absent — never fabricate zero for missing stats. */
  toolCallCount: number | null
  editRollup: EditRollupViewModel
  attentionRequest: AttentionRequestSummary | null
  runtimeStats: DelegationRuntimeStats | null
  filesExpanded: boolean
  onToggleFilesExpanded: () => void
  /**
   * Overlay rows use single-line truncate; the message-stream card uses
   * clamp so long titles still fit without expanding the card height.
   */
  compact?: boolean
  className?: string
}

/** Narrow translator for edit-rollup keys (avoids deep next-intl generics). */
type EditSegmentTranslator = {
  (
    key: "editFilesCount" | "editCallsDetected",
    values: { count: number }
  ): string
  (key: "editFilesTruncated", values: { count: number }): string
  (
    key: "lineTotals",
    values: { additions: number; deletions: number }
  ): string
}

function buildEditSegment(
  editRollup: EditRollupViewModel,
  t: EditSegmentTranslator
): string | null {
  if (editRollup.mode === "files") {
    const countLabel = editRollup.fileCountTruncated
      ? t("editFilesTruncated", { count: editRollup.fileCount })
      : t("editFilesCount", { count: editRollup.fileCount })
    if (
      editRollup.showLineTotals &&
      editRollup.additions != null &&
      editRollup.deletions != null
    ) {
      return `${countLabel} ${t("lineTotals", {
        additions: editRollup.additions,
        deletions: editRollup.deletions,
      })}`
    }
    return countLabel
  }
  if (editRollup.mode === "editCalls") {
    return t("editCallsDetected", { count: editRollup.editCallCount })
  }
  return null
}

export function DelegationCardChrome({
  displaySecondary,
  conversationTitle,
  task,
  elapsedMs,
  toolCallCount,
  editRollup,
  attentionRequest,
  runtimeStats,
  filesExpanded,
  onToggleFilesExpanded,
  compact = false,
  className,
}: DelegationCardChromeProps) {
  const t = useTranslations("Folder.chat.delegation")
  const tLive = useTranslations("Folder.chat.liveTurnStats")
  const filesPanelId = useId()

  const secondaryTooltip =
    (typeof conversationTitle === "string" && conversationTitle.trim()) ||
    (typeof task === "string" && task) ||
    displaySecondary ||
    undefined

  const operationalSegments = useMemo(() => {
    const segments: string[] = []
    if (elapsedMs != null) {
      segments.push(formatElapsedLabel(elapsedMs, tLive))
    }
    if (toolCallCount != null) {
      segments.push(tLive("toolUseCount", { count: toolCallCount }))
    }
    const editSegment = buildEditSegment(editRollup, t)
    if (editSegment) segments.push(editSegment)
    return segments
  }, [elapsedMs, toolCallCount, editRollup, t, tLive])

  const operationalLine =
    operationalSegments.length > 0
      ? operationalSegments.join(" | ")
      : null

  const touchedFiles = runtimeStats?.touched_files ?? []
  const canExpandFiles = touchedFiles.length > 0
  const filesTruncated = runtimeStats?.touched_files_truncated === true

  return (
    <div
      data-testid="delegation-card-chrome"
      className={cn("min-w-0 space-y-1", className)}
    >
      {attentionRequest != null && (
        <Badge
          data-testid="delegation-attention-badge"
          className="gap-1.5 rounded-full text-xs"
          variant="secondary"
          title={attentionRequest.message || undefined}
        >
          <Bell className="h-3 w-3 text-sky-600 dark:text-sky-400" />
          {t("waitingParentDecision")}
        </Badge>
      )}

      {displaySecondary && (
        <div
          data-testid="delegation-secondary"
          className={cn(
            "min-w-0 text-xs text-muted-foreground",
            compact
              ? "truncate"
              : "whitespace-pre-wrap break-words line-clamp-1"
          )}
          title={secondaryTooltip}
        >
          {displaySecondary}
        </div>
      )}

      {operationalLine && (
        <div
          data-testid="delegation-operational"
          className="flex min-w-0 items-start gap-1.5 text-[11px] leading-snug text-muted-foreground"
        >
          <div
            className="min-w-0 flex-1 truncate"
            title={operationalLine}
          >
            <span className="inline-flex max-w-full items-center gap-1">
              {elapsedMs != null && (
                <Timer
                  aria-hidden="true"
                  className="hidden h-3 w-3 shrink-0 sm:inline"
                />
              )}
              {toolCallCount != null && elapsedMs == null && (
                <Wrench
                  aria-hidden="true"
                  className="hidden h-3 w-3 shrink-0 sm:inline"
                />
              )}
              {editRollup.mode !== "omit" &&
                elapsedMs == null &&
                toolCallCount == null && (
                  <FilePenLine
                    aria-hidden="true"
                    className="hidden h-3 w-3 shrink-0 sm:inline"
                  />
                )}
              <span className="truncate">{operationalLine}</span>
            </span>
          </div>
          {canExpandFiles && (
            <button
              type="button"
              data-testid="delegation-files-toggle"
              aria-expanded={filesExpanded}
              aria-controls={filesExpanded ? filesPanelId : undefined}
              onClick={(e) => {
                e.stopPropagation()
                onToggleFilesExpanded()
              }}
              className="inline-flex shrink-0 items-center gap-0.5 rounded px-1 py-0.5 text-[11px] font-medium text-foreground/80 hover:bg-muted/60 hover:text-foreground transition-colors"
              title={
                filesExpanded ? t("hideFileDetails") : t("showFileDetails")
              }
              aria-label={
                filesExpanded ? t("hideFileDetails") : t("showFileDetails")
              }
            >
              {filesExpanded ? (
                <ChevronDown className="h-3 w-3" />
              ) : (
                <ChevronRight className="h-3 w-3" />
              )}
              <span className="sr-only">
                {filesExpanded ? t("hideFileDetails") : t("showFileDetails")}
              </span>
            </button>
          )}
        </div>
      )}

      {/* Expand toggle alone when ops line is empty but paths exist (edge). */}
      {!operationalLine && canExpandFiles && (
        <div className="flex min-w-0 items-center">
          <button
            type="button"
            data-testid="delegation-files-toggle"
            aria-expanded={filesExpanded}
            aria-controls={filesExpanded ? filesPanelId : undefined}
            onClick={(e) => {
              e.stopPropagation()
              onToggleFilesExpanded()
            }}
            className="inline-flex shrink-0 items-center gap-0.5 rounded px-1 py-0.5 text-[11px] font-medium text-foreground/80 hover:bg-muted/60 hover:text-foreground transition-colors"
            title={
              filesExpanded ? t("hideFileDetails") : t("showFileDetails")
            }
            aria-label={
              filesExpanded ? t("hideFileDetails") : t("showFileDetails")
            }
          >
            {filesExpanded ? (
              <ChevronDown className="h-3 w-3" />
            ) : (
              <ChevronRight className="h-3 w-3" />
            )}
            <span>
              {filesExpanded ? t("hideFileDetails") : t("showFileDetails")}
            </span>
          </button>
        </div>
      )}

      {canExpandFiles && filesExpanded && (
        <div
          id={filesPanelId}
          data-testid="delegation-files-panel"
          className="space-y-1 rounded-md border border-border/60 bg-muted/30 px-2 py-1.5"
        >
          <div className="text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
            {t("expandedFilesHeading")}
          </div>
          <ul className="space-y-0.5">
            {touchedFiles.map((file) => {
              const hasLineCounts =
                file.additions != null && file.deletions != null
              return (
                <li
                  key={file.path}
                  data-testid="delegation-touched-file"
                  className="min-w-0 text-[11px] leading-snug text-foreground/90"
                >
                  <div className="flex min-w-0 flex-wrap items-baseline gap-x-1.5 gap-y-0.5">
                    <span
                      className="min-w-0 break-all font-mono"
                      title={file.path}
                    >
                      {file.path}
                    </span>
                    {file.outside_workspace && (
                      <span
                        data-testid="delegation-outside-workspace"
                        className="shrink-0 rounded bg-amber-500/10 px-1 py-px text-[10px] font-medium text-amber-700 dark:text-amber-400"
                      >
                        {t("outsideWorkspace")}
                      </span>
                    )}
                    {hasLineCounts && (
                      <span className="shrink-0 tabular-nums text-muted-foreground">
                        {t("lineTotals", {
                          additions: file.additions,
                          deletions: file.deletions,
                        })}
                      </span>
                    )}
                  </div>
                </li>
              )
            })}
          </ul>
          {filesTruncated && (
            <div
              data-testid="delegation-files-truncated"
              className="text-[10px] text-muted-foreground"
            >
              {t("filesTruncatedNotice", { count: touchedFiles.length })}
            </div>
          )}
        </div>
      )}
    </div>
  )
}
