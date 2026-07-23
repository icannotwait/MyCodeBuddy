"use client"

import { useCallback, useEffect, useMemo, useState } from "react"
import { AlertTriangle, Clock3, Hand, Hourglass } from "lucide-react"
import { useTranslations } from "next-intl"
import { toast } from "sonner"
import { Button } from "@/components/ui/button"
import { cancelToolWatchdogLease, extendToolWatchdogLease } from "@/lib/api"
import { extractAppCommandError } from "@/lib/app-error"
import type { ToolWatchdogProjection, ToolWatchdogTitle } from "@/lib/types"
import {
  formatCountdown,
  remainingGraceSeconds,
} from "@/lib/tool-watchdog-projection"
import { cn } from "@/lib/utils"
import { useConnection } from "@/hooks/use-connection"

export {
  formatCountdown,
  reduceToolWatchdogProjection,
  remainingGraceSeconds,
} from "@/lib/tool-watchdog-projection"

const STALE_LEASE_CODE = "stale_tool_watchdog_lease"

/** Phases that keep a banner surface open. */
const ACTIONABLE_PHASES = new Set(["warning", "grace", "cancelling"])

const TITLE_I18N_KEY = {
  terminal: "titleTerminal",
  delegation: "titleDelegation",
  mcp: "titleMcp",
  other: "titleOther",
} as const satisfies Record<ToolWatchdogTitle, string>

function formatRelativeProgress(
  iso: string,
  nowMs: number,
  unknownProgress: string,
  formatAgo: (unit: "seconds" | "minutes" | "hours", n: number) => string
): string {
  const parsed = Date.parse(iso)
  if (Number.isNaN(parsed)) return unknownProgress
  const deltaSec = Math.max(0, Math.floor((nowMs - parsed) / 1000))
  if (deltaSec < 60) return formatAgo("seconds", deltaSec)
  const mins = Math.floor(deltaSec / 60)
  if (mins < 60) return formatAgo("minutes", mins)
  const hours = Math.floor(mins / 60)
  return formatAgo("hours", hours)
}

function pickVisibleProjections(
  map: Record<string, ToolWatchdogProjection> | undefined | null
): ToolWatchdogProjection[] {
  if (!map) return []
  return Object.values(map)
    .filter((p) => ACTIONABLE_PHASES.has(p.phase))
    .sort((a, b) => a.lease_id.localeCompare(b.lease_id))
}

interface LeaseBannerRowProps {
  projection: ToolWatchdogProjection
  nowMs: number
  pending: boolean
  onStop: (p: ToolWatchdogProjection) => void
  onWait: (p: ToolWatchdogProjection) => void
}

function LeaseBannerRow({
  projection,
  nowMs,
  pending,
  onStop,
  onWait,
}: LeaseBannerRowProps) {
  const t = useTranslations("ToolWatchdogBanner")
  const remaining = remainingGraceSeconds(projection.grace_deadline, nowMs)
  const cancelling = projection.phase === "cancelling"
  const actionsDisabled = pending || cancelling
  // Extend is Grace-only on the backend.
  const waitDisabled = actionsDisabled || projection.phase !== "grace"
  const title = t(TITLE_I18N_KEY[projection.tool_title])

  return (
    <div
      className="@container border-b border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-700 dark:text-amber-300"
      data-testid="tool-watchdog-banner"
      data-lease-id={projection.lease_id}
      data-version={projection.version}
      data-phase={projection.phase}
    >
      <div className="mx-auto flex w-full max-w-3xl flex-col gap-1.5 @lg:flex-row @lg:items-center @lg:gap-2">
        <div className="flex min-w-0 flex-1 items-start gap-2">
          <AlertTriangle className="mt-0.5 size-3.5 shrink-0" aria-hidden />
          <div className="min-w-0 space-y-0.5">
            <div className="font-medium break-words">
              {t("appearsStalled", { title })}
            </div>
            <div className="flex flex-wrap items-center gap-x-3 gap-y-0.5 text-[11px] opacity-90">
              <span className="inline-flex items-center gap-1">
                <Clock3 className="size-3" aria-hidden />
                {t("lastProgress", {
                  when: formatRelativeProgress(
                    projection.last_progress_at,
                    nowMs,
                    t("unknownProgress"),
                    (unit, n) =>
                      unit === "seconds"
                        ? t("secondsAgo", { n })
                        : unit === "minutes"
                          ? t("minutesAgo", { n })
                          : t("hoursAgo", { n })
                  ),
                })}
              </span>
              {remaining != null && (
                <span
                  className="inline-flex items-center gap-1 tabular-nums"
                  data-testid="tool-watchdog-countdown"
                >
                  <Hourglass className="size-3" aria-hidden />
                  {t("grace", { countdown: formatCountdown(remaining) })}
                </span>
              )}
              {cancelling && (
                <span className="inline-flex items-center gap-1">
                  {t("stopping")}
                </span>
              )}
            </div>
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-1.5 self-end @lg:self-center">
          <Button
            type="button"
            size="sm"
            variant="outline"
            className={cn(
              "h-7 border-amber-500/40 bg-background/60 px-2 text-xs",
              "hover:bg-amber-500/15"
            )}
            disabled={actionsDisabled}
            onClick={() => onStop(projection)}
            data-testid="tool-watchdog-stop"
          >
            <Hand className="size-3" aria-hidden />
            {t("stopNow")}
          </Button>
          <Button
            type="button"
            size="sm"
            variant="outline"
            className={cn(
              "h-7 border-amber-500/40 bg-background/60 px-2 text-xs",
              "hover:bg-amber-500/15"
            )}
            disabled={waitDisabled}
            onClick={() => onWait(projection)}
            data-testid="tool-watchdog-wait"
          >
            {t("wait10Minutes")}
          </Button>
        </div>
      </div>
    </div>
  )
}

/**
 * Persistent in-session surface for host tool-execution watchdog warnings.
 * Renders near the transcript/composer boundary (via session surface
 * `topBanner`), not as a toast and not inside a tool card.
 *
 * Controls send `lease_id` + current `version`, then disable until the next
 * authoritative projection event for that lease. All windows reduce the same
 * backend version — no local terminal outcome is invented here.
 */
export function ToolWatchdogBanner({ contextKey }: { contextKey: string }) {
  const t = useTranslations("ToolWatchdogBanner")
  const { toolWatchdogProjections } = useConnection(contextKey)
  const visible = useMemo(
    () => pickVisibleProjections(toolWatchdogProjections),
    [toolWatchdogProjections]
  )

  // Tick countdown every second while any grace deadline is open.
  const [nowMs, setNowMs] = useState(() => Date.now())
  useEffect(() => {
    if (visible.length === 0) return
    const id = window.setInterval(() => setNowMs(Date.now()), 1000)
    return () => window.clearInterval(id)
  }, [visible.length])

  // Per-lease pending click: value is the version that was clicked. Controls
  // stay disabled while the live projection still reports that exact version.
  // When the authoritative next event advances version (or removes the lease),
  // `pendingByLease[id] === version` becomes false without a clear effect.
  const [pendingByLease, setPendingByLease] = useState<Record<string, number>>(
    {}
  )

  const markPending = useCallback((p: ToolWatchdogProjection) => {
    setPendingByLease((prev) => {
      if (prev[p.lease_id] === p.version) return prev
      return { ...prev, [p.lease_id]: p.version }
    })
  }, [])

  const onStop = useCallback(
    async (p: ToolWatchdogProjection) => {
      if (pendingByLease[p.lease_id] === p.version) return
      markPending(p)
      try {
        await cancelToolWatchdogLease(p.lease_id, p.version)
      } catch (error) {
        const appErr = extractAppCommandError(error)
        const msg =
          appErr?.detail ||
          appErr?.message ||
          (error instanceof Error ? error.message : String(error))
        // Stale CAS: clear pending so the user can retry after live events
        // refresh the version (multi-window loser path).
        if (
          appErr?.message?.includes(STALE_LEASE_CODE) ||
          msg.includes(STALE_LEASE_CODE)
        ) {
          setPendingByLease((prev) => {
            if (!(p.lease_id in prev)) return prev
            const next = { ...prev }
            delete next[p.lease_id]
            return next
          })
        }
        toast.error(t("stopFailed"), { description: msg })
      }
    },
    [markPending, pendingByLease, t]
  )

  const onWait = useCallback(
    async (p: ToolWatchdogProjection) => {
      if (pendingByLease[p.lease_id] === p.version) return
      markPending(p)
      try {
        await extendToolWatchdogLease(p.lease_id, p.version)
      } catch (error) {
        const appErr = extractAppCommandError(error)
        const msg =
          appErr?.detail ||
          appErr?.message ||
          (error instanceof Error ? error.message : String(error))
        if (
          appErr?.message?.includes(STALE_LEASE_CODE) ||
          msg.includes(STALE_LEASE_CODE)
        ) {
          setPendingByLease((prev) => {
            if (!(p.lease_id in prev)) return prev
            const next = { ...prev }
            delete next[p.lease_id]
            return next
          })
        }
        toast.error(t("extendFailed"), { description: msg })
      }
    },
    [markPending, pendingByLease, t]
  )

  if (visible.length === 0) return null

  return (
    <>
      {visible.map((p) => (
        <LeaseBannerRow
          key={p.lease_id}
          projection={p}
          nowMs={nowMs}
          pending={pendingByLease[p.lease_id] === p.version}
          onStop={onStop}
          onWait={onWait}
        />
      ))}
    </>
  )
}
