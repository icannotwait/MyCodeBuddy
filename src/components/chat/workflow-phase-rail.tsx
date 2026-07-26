"use client"

/**
 * Compact four-step Design → Plan → Tasks → Final phase rail for the
 * workflow overlay. Gate chrome uses B11 required-reviewer counts only.
 */

import { memo } from "react"
import { useTranslations } from "next-intl"

import type { PhaseRailItem } from "@/lib/workflow-graph-store"
import { cn } from "@/lib/utils"

interface WorkflowPhaseRailProps {
  phases: PhaseRailItem[]
  className?: string
}

export const WorkflowPhaseRail = memo(function WorkflowPhaseRail({
  phases,
  className,
}: WorkflowPhaseRailProps) {
  const t = useTranslations("Folder.chat.workflowGraph")

  return (
    <ol
      className={cn(
        "flex w-full min-w-0 items-stretch gap-1",
        className
      )}
      data-testid="workflow-phase-rail"
      aria-label={t("phaseRailAria")}
    >
      {phases.map((phase, index) => (
        <li
          key={phase.kind}
          className="flex min-w-0 flex-1 items-center gap-1"
          data-testid={`workflow-phase-${phase.kind}`}
          data-status={phase.status}
        >
          <div
            className={cn(
              "flex min-w-0 flex-1 flex-col gap-0.5 rounded-md border px-1.5 py-1",
              phaseStatusClass(phase.status)
            )}
          >
            <span className="truncate text-[10px] font-semibold uppercase tracking-wide">
              {t(`phase.${phase.kind}`)}
            </span>
            <span className="truncate text-[10px] text-muted-foreground">
              {t(`phaseStatus.${phase.status}`)}
            </span>
            {phase.gate != null && phase.gate.required > 0 && (
              <span
                className="truncate text-[10px] tabular-nums"
                data-testid={`workflow-phase-gate-${phase.kind}`}
              >
                {t("gateProgress", {
                  returned: phase.gate.returned,
                  required: phase.gate.required,
                })}
                {phase.gate.running > 0
                  ? ` · ${t("gateRunning", { count: phase.gate.running })}`
                  : ""}
                {phase.gate.blocked > 0
                  ? ` · ${t("gateBlocked", { count: phase.gate.blocked })}`
                  : ""}
              </span>
            )}
            {phase.taskProgress != null && (
              <span
                className="truncate text-[10px] tabular-nums"
                data-testid="workflow-phase-task-progress"
              >
                {t("taskProgress", {
                  current: phase.taskProgress.current,
                  total: phase.taskProgress.total,
                })}
              </span>
            )}
          </div>
          {index < phases.length - 1 && (
            <span
              aria-hidden
              className="shrink-0 text-[10px] text-muted-foreground"
            >
              →
            </span>
          )}
        </li>
      ))}
    </ol>
  )
})

function phaseStatusClass(status: PhaseRailItem["status"]): string {
  switch (status) {
    case "completed":
      return "border-emerald-500/40 bg-emerald-500/10 text-emerald-700 dark:text-emerald-300"
    case "current":
      return "border-blue-500/50 bg-blue-500/10 text-blue-700 dark:text-blue-300"
    case "blocked":
      return "border-destructive/40 bg-destructive/10 text-destructive"
    case "estimated":
      return "border-dashed border-muted-foreground/30 bg-muted/40 text-muted-foreground"
    default:
      return "border-border/60 bg-muted/20 text-muted-foreground"
  }
}
