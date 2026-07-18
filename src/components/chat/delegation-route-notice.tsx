"use client"

/**
 * Compact warning for degraded / unavailable Codeg routes. Normal Codeg and
 * native routes render nothing. Action is explicit reconnect only — never
 * automatic switching.
 */

import { useState } from "react"
import { useTranslations } from "next-intl"
import { AlertTriangle, RefreshCw } from "lucide-react"
import { toast } from "sonner"

import { Button } from "@/components/ui/button"
import { useConnection } from "@/hooks/use-connection"
import type { DelegationRouteSnapshot } from "@/lib/types"
import { cn } from "@/lib/utils"

export function shouldShowRouteNotice(
  route: DelegationRouteSnapshot | null | undefined
): boolean {
  if (!route) return false
  if (route.source === "safe_fallback" || route.degraded_reason) return true
  if (route.effective === "codeg" && !route.delegation_available) return true
  return false
}

export function DelegationRouteNotice({ contextKey }: { contextKey: string }) {
  const t = useTranslations("Folder.chat.delegationRouteNotice")
  const {
    delegationRoute,
    isViewer,
    isDelegationChild,
    status,
    reapplyConfig,
  } = useConnection(contextKey)
  const [reconnecting, setReconnecting] = useState(false)

  if (
    !shouldShowRouteNotice(delegationRoute) ||
    isViewer ||
    isDelegationChild
  ) {
    return null
  }

  const route = delegationRoute!
  const turnInFlight = status === "prompting"
  const busy = reconnecting || status === "connecting"
  const actionDisabled = turnInFlight || busy

  let message: string
  if (route.source === "safe_fallback" || route.degraded_reason) {
    message = t("safeFallback", {
      reason: route.degraded_reason
        ? t(`reason.${route.degraded_reason}`)
        : t("reason.unknown"),
    })
  } else {
    message = t("delegationUnavailable")
  }

  const handleReconnect = async () => {
    if (actionDisabled) return
    setReconnecting(true)
    try {
      const ok = await reapplyConfig()
      if (ok) toast.success(t("reconnected"))
    } catch (error) {
      toast.error(t("reconnectFailed"), {
        description: error instanceof Error ? error.message : String(error),
      })
    } finally {
      setReconnecting(false)
    }
  }

  return (
    <div className="@container border-b border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-700 dark:text-amber-300">
      <div className="mx-auto flex w-full max-w-3xl flex-col gap-1.5 @lg:flex-row @lg:items-center @lg:gap-2">
        <div className="flex min-w-0 flex-1 items-start gap-2">
          <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-amber-600 dark:text-amber-400 @lg:mt-0" />
          <p className="min-w-0 leading-snug">{message}</p>
        </div>
        <div className="flex shrink-0 items-center gap-1 self-end @lg:self-auto">
          <Button
            size="sm"
            variant="outline"
            className="h-7 gap-1.5 border-amber-500/40 bg-transparent text-amber-700 hover:bg-amber-500/20 hover:text-amber-800 dark:text-amber-300 dark:hover:text-amber-200"
            disabled={actionDisabled}
            onClick={handleReconnect}
          >
            <RefreshCw className={cn("h-3.5 w-3.5", busy && "animate-spin")} />
            {busy ? t("reconnecting") : t("reconnect")}
          </Button>
        </div>
      </div>
    </div>
  )
}
