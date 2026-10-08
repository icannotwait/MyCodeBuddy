"use client"

import Link from "next/link"
import { useId } from "react"
import { useTranslations } from "next-intl"
import { FolderOpen, Loader2, TriangleAlert } from "lucide-react"
import { Button } from "@/components/ui/button"
import { formatFolderLabelWithAlias } from "@/lib/folder-display"
import type { FolderDetail } from "@/lib/types"
import { cn } from "@/lib/utils"
import { SELECT_CLASS } from "./roundtable-composer"

export type RoundtableWorkspace = Pick<
  FolderDetail,
  "id" | "name" | "path" | "alias" | "parent_id"
>

/** Roots first, each followed by its worktrees, keeping the given order. */
export function orderRoundtableWorkspaces<T extends RoundtableWorkspace>(
  workspaces: readonly T[]
): { workspace: T; nested: boolean }[] {
  const ids = new Set(workspaces.map((workspace) => workspace.id))
  const roots = workspaces.filter(
    (workspace) => workspace.parent_id == null || !ids.has(workspace.parent_id)
  )
  return roots.flatMap((root) => [
    { workspace: root, nested: false },
    ...workspaces
      .filter((workspace) => workspace.parent_id === root.id)
      .map((workspace) => ({ workspace, nested: true })),
  ])
}

export function RoundtableWorkspaceSelect({
  workspaces,
  loading,
  error,
  value,
  disabled,
  onChange,
  onRetry,
}: {
  workspaces: RoundtableWorkspace[] | null
  loading: boolean
  error: string | null
  value: string
  disabled?: boolean
  onChange: (workspaceId: string) => void
  onRetry: () => void
}) {
  const t = useTranslations("Roundtable")
  const uid = useId()
  const statusId = `${uid}-status`
  const list = workspaces ?? []
  const selected = list.find((workspace) => String(workspace.id) === value)
  const unknown = !!value && !!workspaces && !selected
  const empty = !!workspaces && list.length === 0 && !error

  return (
    <div className="flex min-w-0 flex-col gap-2">
      <div className="flex min-w-0 items-center gap-2">
        <FolderOpen
          aria-hidden="true"
          className="size-4 shrink-0 text-muted-foreground"
        />
        <select
          aria-label={t("workspace")}
          aria-invalid={unknown || (!value && !!workspaces) || undefined}
          aria-describedby={statusId}
          aria-busy={loading || undefined}
          className={cn(SELECT_CLASS, "bg-background font-medium")}
          disabled={disabled || loading || (!workspaces && !value)}
          value={value}
          onChange={(event) => onChange(event.target.value)}
        >
          {!value ? (
            <option value="" disabled>
              {loading ? t("workspacesLoading") : t("workspacePlaceholder")}
            </option>
          ) : null}
          {value && !selected ? (
            <option value={value}>
              {loading || !workspaces
                ? `#${value}`
                : t("workspaceUnknown", { id: value })}
            </option>
          ) : null}
          {orderRoundtableWorkspaces(list).map(({ workspace, nested }) => (
            <option key={workspace.id} value={String(workspace.id)}>
              {`${nested ? "\u00a0\u00a0↳ " : ""}${formatFolderLabelWithAlias(workspace)}`}
            </option>
          ))}
        </select>
        {loading ? (
          <Loader2
            aria-hidden="true"
            className="size-4 shrink-0 animate-spin text-muted-foreground"
          />
        ) : null}
      </div>
      <div id={statusId} className="min-w-0 text-xs leading-relaxed">
        {error ? (
          <div
            role="alert"
            className="flex flex-wrap items-center gap-2 text-destructive"
          >
            <TriangleAlert aria-hidden="true" className="size-3.5 shrink-0" />
            <span className="[overflow-wrap:anywhere]">
              {t("workspacesError")} {error}
            </span>
            <Button size="sm" variant="outline" onClick={onRetry}>
              {t("retryWorkspaces")}
            </Button>
          </div>
        ) : loading ? (
          <p role="status" className="text-muted-foreground">
            {t("workspacesLoading")}
          </p>
        ) : empty ? (
          <p role="status" className="text-muted-foreground">
            {t("workspacesEmpty")}{" "}
            <Link
              href="/workspace"
              className="font-medium text-foreground underline underline-offset-2"
            >
              {t("openFolder")}
            </Link>
          </p>
        ) : unknown ? (
          <p role="alert" className="text-destructive">
            {t("workspaceUnavailable")}
          </p>
        ) : !value && list.length > 0 ? (
          <p className="text-muted-foreground">{t("workspaceRequired")}</p>
        ) : selected ? (
          <p
            className="truncate font-mono text-muted-foreground"
            title={selected.path}
          >
            {selected.path}
          </p>
        ) : null}
      </div>
    </div>
  )
}
