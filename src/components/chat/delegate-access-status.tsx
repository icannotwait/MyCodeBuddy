"use client"

import {
  CircleCheck,
  Eye,
  LoaderCircle,
  LockKeyhole,
  ShieldAlert,
  TriangleAlert,
  type LucideIcon,
} from "lucide-react"
import { useTranslations } from "next-intl"
import type { DelegateAccessState } from "@/lib/types"
import { cn } from "@/lib/utils"

export type DelegateAccessStatusKind =
  | "waiting"
  | "observing"
  | "parent_turn_active"
  | "state_unknown"
  | "interactive"
  | "sync_failed"

export interface DelegateAccessStatusProps {
  access: DelegateAccessState
  loading: boolean
  connectionId: string | null
  syncError: string | null
}

export function resolveDelegateAccessStatus({
  access,
  loading,
  connectionId,
  syncError,
}: DelegateAccessStatusProps): DelegateAccessStatusKind {
  // Empty string is still a failure signal (highest priority); do not use truthiness.
  if (syncError !== null) return "sync_failed"
  if (loading || access.reason === "state_unknown") return "state_unknown"
  if (access.mode === "interactive" && access.reason === null) {
    return "interactive"
  }
  if (access.mode !== "viewer_only") return "state_unknown"
  if (access.reason === "task_running") {
    return connectionId ? "observing" : "waiting"
  }
  if (access.reason === "parent_turn_active") return "parent_turn_active"
  return "state_unknown"
}

type DelegateAccessMessageKey =
  | "waiting"
  | "observing"
  | "parentTurnActive"
  | "stateUnknown"
  | "interactive"
  | "syncFailed"

const PRESENTATION: Record<
  DelegateAccessStatusKind,
  {
    icon: LucideIcon
    messageKey: DelegateAccessMessageKey
    tone: string
    spin?: boolean
  }
> = {
  waiting: {
    icon: LoaderCircle,
    messageKey: "waiting",
    tone: "text-muted-foreground",
    spin: true,
  },
  observing: {
    icon: Eye,
    messageKey: "observing",
    tone: "text-muted-foreground",
  },
  parent_turn_active: {
    icon: LockKeyhole,
    messageKey: "parentTurnActive",
    tone: "text-amber-700 dark:text-amber-300",
  },
  state_unknown: {
    icon: ShieldAlert,
    messageKey: "stateUnknown",
    tone: "text-amber-700 dark:text-amber-300",
  },
  interactive: {
    icon: CircleCheck,
    messageKey: "interactive",
    tone: "text-emerald-700 dark:text-emerald-300",
  },
  sync_failed: {
    icon: TriangleAlert,
    messageKey: "syncFailed",
    tone: "text-destructive",
  },
}

export function DelegateAccessStatus(props: DelegateAccessStatusProps) {
  const t = useTranslations("Folder.chat.delegateAccess")
  const kind = resolveDelegateAccessStatus(props)
  const { icon: Icon, messageKey, tone, spin } = PRESENTATION[kind]

  return (
    <div
      role={kind === "sync_failed" ? "alert" : "status"}
      aria-live="polite"
      data-state={kind}
      title={props.syncError ?? undefined}
      className={cn(
        "flex h-8 w-full items-center gap-2 border-b bg-muted/30 px-4 text-xs",
        tone
      )}
    >
      <Icon
        aria-hidden="true"
        className={cn("h-3.5 w-3.5 shrink-0", spin && "animate-spin")}
      />
      <span className="min-w-0 truncate">{t(messageKey)}</span>
    </div>
  )
}
