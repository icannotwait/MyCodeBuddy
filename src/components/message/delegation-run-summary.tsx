"use client"

import { useTranslations } from "next-intl"

import type { CardSummary } from "@/lib/types"

interface Props {
  summary: CardSummary | null
  compact?: boolean
}

export function DelegationRunSummary({ summary, compact = false }: Props) {
  const t = useTranslations("Folder.chat.delegation")
  if (!summary) return null

  if (summary.kind === "review") {
    return (
      <section
        data-testid="delegation-run-summary"
        dir="auto"
        className="min-w-0 border-t border-border/60 pt-2"
      >
        <div className="flex min-w-0 flex-col gap-1 sm:flex-row sm:items-start sm:justify-between">
          <span className="break-words text-xs font-medium text-foreground">
            {t("summary.review", {
              verdict: t(`summary.verdict.${summary.verdict}`),
            })}
          </span>
          <div className="flex flex-wrap gap-x-2 gap-y-0.5 text-[11px] tabular-nums text-muted-foreground">
            <span>{t("summary.critical", { count: summary.critical })}</span>
            <span>{t("summary.important", { count: summary.important })}</span>
            <span>{t("summary.minor", { count: summary.minor })}</span>
          </div>
        </div>
        <p className="mt-1 break-words text-xs leading-snug text-muted-foreground">
          {summary.summary}
        </p>
      </section>
    )
  }

  if (summary.kind === "author") {
    return (
      <section
        data-testid="delegation-run-summary"
        dir="auto"
        className="min-w-0 border-t border-border/60 pt-2"
      >
        <span className="break-words text-xs font-medium text-foreground">
          {t("summary.author", {
            status: t(`summary.workStatus.${summary.status}`),
          })}
        </span>
        <p className="mt-1 break-words text-xs leading-snug text-muted-foreground">
          {summary.summary}
        </p>
      </section>
    )
  }

  const commits = summary.commits ?? []
  const concerns = summary.concerns ?? []
  return (
    <section
      data-testid="delegation-run-summary"
      dir="auto"
      className="min-w-0 border-t border-border/60 pt-2"
    >
      <div className="flex min-w-0 flex-col gap-1 sm:flex-row sm:items-start sm:justify-between">
        <span className="break-words text-xs font-medium text-foreground">
          {t("summary.implementation", {
            status: t(`summary.workStatus.${summary.status}`),
          })}
        </span>
        {concerns.length > 0 && (
          <span className="shrink-0 text-[11px] font-medium text-amber-700 dark:text-amber-400">
            {t("summary.concerns", { count: concerns.length })}
          </span>
        )}
      </div>
      <p className="mt-1 break-words text-xs leading-snug text-muted-foreground">
        {summary.summary}
      </p>
      {!compact &&
        (commits.length > 0 || summary.tests || concerns.length > 0) && (
          <div className="mt-1.5 flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 text-[11px] text-muted-foreground">
            {commits.length > 0 && (
              <span className="min-w-0 break-words">
                {t("summary.commits", {
                  commits: commits
                    .map((commit) => commit.sha.slice(0, 8))
                    .join(", "),
                })}
              </span>
            )}
            {summary.tests && (
              <span className="min-w-0 break-words">
                {t("summary.tests", {
                  status: summary.tests.status,
                  passed: summary.tests.passed ?? 0,
                  failed: summary.tests.failed ?? 0,
                })}
              </span>
            )}
          </div>
        )}
    </section>
  )
}
