"use client"

import { CheckIcon, CircleDashedIcon, Clock3Icon, XIcon } from "lucide-react"

import { cn } from "@/lib/utils"

export interface WorkflowStatusIconProps {
  visualStatus: string
  className?: string
}

type StatusBucket =
  | "completed"
  | "active"
  | "blocked"
  | "waiting"
  | "inactive"
  | "reserving"
  | "estimated"

function statusBucket(visualStatus: string): StatusBucket {
  switch (visualStatus) {
    case "completed":
      return "completed"
    case "current":
    case "running":
      return "active"
    case "blocked":
    case "failed":
    case "missing_summary":
      return "blocked"
    case "waiting_review":
    case "waiting_adjudication":
      return "waiting"
    case "reserving":
      return "reserving"
    case "estimated":
      return "estimated"
    case "canceled":
    case "pending":
    case "superseded":
    default:
      return "inactive"
  }
}

export function WorkflowStatusIcon({
  visualStatus,
  className,
}: WorkflowStatusIconProps) {
  const bucket = statusBucket(visualStatus)
  const rootClass = cn(
    "relative inline-flex size-4 shrink-0 items-center justify-center",
    bucket === "active" &&
      "motion-safe:animate-pulse text-blue-600 dark:text-blue-400",
    bucket === "completed" &&
      "rounded-full bg-emerald-600 text-white dark:bg-emerald-500",
    bucket === "blocked" &&
      "rounded-full border border-destructive text-destructive",
    (bucket === "waiting" || bucket === "reserving") &&
      "text-amber-600 dark:text-amber-400",
    bucket === "inactive" && "text-muted-foreground",
    bucket === "estimated" && "text-muted-foreground",
    className
  )

  return (
    <span
      aria-hidden="true"
      className={rootClass}
      data-testid="workflow-status-icon"
      data-visual-status={visualStatus}
      data-status-bucket={bucket}
    >
      {bucket === "completed" && <CheckIcon className="size-3" />}
      {bucket === "active" && (
        <span className="size-2 rounded-full bg-current" />
      )}
      {bucket === "blocked" && <XIcon className="size-3" />}
      {bucket === "waiting" && (
        <span className="relative size-3 rounded-full border border-current">
          <span className="absolute inset-1 rounded-full bg-current" />
        </span>
      )}
      {bucket === "inactive" && (
        <span className="size-3 rounded-full border border-current" />
      )}
      {bucket === "reserving" && <Clock3Icon className="size-4" />}
      {bucket === "estimated" && <CircleDashedIcon className="size-4" />}
    </span>
  )
}
