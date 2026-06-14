"use client"

import { useEffect, useState } from "react"
import { useTranslations } from "next-intl"
import { getLoopEngineHealth } from "@/lib/loops-api"
import type { LoopEngineHealth } from "@/lib/types"

/**
 * A quiet pill in the workbench corner showing live engine health: running
 * issues, in-flight iterations, and a soft amber warning when token
 * settlements are piling up `pending`. Self-contained and resilient — it polls
 * on a light interval and swallows transport errors so it never throws into the
 * tree. (A global engine snapshot has no single `loop://changed` subject to
 * subscribe to, and `activeDrivers` is registry-derived without a DB event, so
 * a steady poll is the right fit rather than the realtime provider.)
 */
export function EngineHealthBadge() {
  const t = useTranslations("Loops.engineHealth")
  const [health, setHealth] = useState<LoopEngineHealth | null>(null)

  useEffect(() => {
    let alive = true
    const tick = () =>
      getLoopEngineHealth()
        .then((h) => {
          if (alive) setHealth(h)
        })
        .catch(() => {})
    tick()
    const id = setInterval(tick, 5000)
    return () => {
      alive = false
      clearInterval(id)
    }
  }, [])

  if (!health) return null

  return (
    <div className="flex items-center gap-2 rounded-full border px-3 py-1 text-xs text-muted-foreground">
      <span className="h-1.5 w-1.5 rounded-full bg-primary" />
      <span>
        {t("summary", {
          issues: health.runningIssues,
          iters: health.inFlightIterations,
        })}
      </span>
      {health.pendingTokenIterations > 0 && (
        <span className="text-amber-600">
          {t("pending", { n: health.pendingTokenIterations })}
        </span>
      )}
    </div>
  )
}
