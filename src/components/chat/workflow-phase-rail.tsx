"use client"

/**
 * Controlled horizontal Design → Plan → Tasks → Final phase rail.
 * Progress lives in aria/title; the summary sibling owns visible status text.
 */

import { memo } from "react"
import { useTranslations } from "next-intl"

import { WorkflowStatusIcon } from "@/components/chat/workflow-status-icon"
import type { PhaseRailItem, PhaseRailKind } from "@/lib/workflow-graph-store"
import { cn } from "@/lib/utils"

interface WorkflowPhaseRailProps {
  phases: PhaseRailItem[]
  selectedKind: PhaseRailKind
  onSelectKind: (kind: PhaseRailKind) => void
  className?: string
}

export const WorkflowPhaseRail = memo(function WorkflowPhaseRail({
  phases,
  selectedKind,
  onSelectKind,
  className,
}: WorkflowPhaseRailProps) {
  const t = useTranslations("Folder.chat.workflowGraph")

  return (
    <ol
      className={cn("flex w-full min-w-0 items-center", className)}
      data-testid="workflow-phase-rail"
      aria-label={t("phaseRailAria")}
    >
      {phases.map((phase, index) => {
        const progress = phaseProgressText(phase, t)
        return (
          <li key={phase.kind} className="flex min-w-0 flex-1 items-center">
            <button
              type="button"
              className="flex min-w-0 flex-col items-center gap-1 px-1 py-1 text-center text-xs"
              data-testid={`workflow-phase-${phase.kind}`}
              data-status={phase.status}
              data-selected={selectedKind === phase.kind ? "true" : "false"}
              aria-current={phase.status === "current" ? "step" : undefined}
              aria-label={t("phaseProgressAria", {
                phase: t(`phase.${phase.kind}`),
                status: t(`phaseStatus.${phase.status}`),
                progress,
              })}
              title={t("phaseProgressAria", {
                phase: t(`phase.${phase.kind}`),
                status: t(`phaseStatus.${phase.status}`),
                progress,
              })}
              onClick={() => onSelectKind(phase.kind)}
            >
              <WorkflowStatusIcon visualStatus={phase.status} />
              <span data-phase-label className="text-xs leading-tight">
                {t(`phase.${phase.kind}`)}
              </span>
            </button>
            {index < phases.length - 1 && (
              <span
                aria-hidden="true"
                className={cn(
                  "mx-1 h-px min-w-2 flex-1 bg-border",
                  phase.status === "completed" && "bg-emerald-500",
                  phase.status === "current" && "bg-blue-500"
                )}
              />
            )}
          </li>
        )
      })}
    </ol>
  )
})

type WorkflowGraphTranslate = ReturnType<
  typeof useTranslations<"Folder.chat.workflowGraph">
>

/** Shared gate/task progress fragments for aria and the summary sibling. */
export function phaseProgressFragments(
  phase: PhaseRailItem,
  t: WorkflowGraphTranslate
): string[] {
  if (phase.gate != null && phase.gate.required > 0) {
    const parts = [
      t("gateProgress", {
        returned: phase.gate.returned,
        required: phase.gate.required,
      }),
    ]
    if (phase.gate.running > 0) {
      parts.push(t("gateRunning", { count: phase.gate.running }))
    }
    if (phase.gate.blocked > 0) {
      parts.push(t("gateBlocked", { count: phase.gate.blocked }))
    }
    return parts
  }
  if (phase.taskProgress != null) {
    return [
      t("taskProgress", {
        current: phase.taskProgress.current,
        total: phase.taskProgress.total,
      }),
    ]
  }
  return []
}

function phaseProgressText(
  phase: PhaseRailItem,
  t: WorkflowGraphTranslate
): string {
  const fragments = phaseProgressFragments(phase, t)
  if (fragments.length > 0) return fragments.join(" · ")
  return t(`phaseStatus.${phase.status}`)
}
