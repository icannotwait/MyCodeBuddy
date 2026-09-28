"use client"

import { useEffect } from "react"
import { useTranslations } from "next-intl"
import { toast } from "sonner"
import { installWorkspaceWebPopoutClaimListener } from "@/lib/conversation-popout-web-presence"
import { releaseWorkspaceTabsBlockingWebPopout } from "@/lib/conversation-popout"
import { useTabStore } from "@/stores/tab-store"

/**
 * Workspace-only: while a web pop-out owns a conversation, drop that tab here
 * and tell the user when a reopen could not focus the existing window.
 */
export function WebPopoutWorkspaceGuard() {
  const t = useTranslations("ConversationPopout")
  const refusal = useTabStore((s) => s.webPopoutReopenRefusal)
  const rawTabs = useTabStore((s) => s.rawTabs)

  useEffect(() => {
    if (!refusal) return
    toast.error(t("popOutStillOpen"))
  }, [refusal, t])

  useEffect(() => {
    releaseWorkspaceTabsBlockingWebPopout()
  }, [rawTabs])

  useEffect(
    () =>
      installWorkspaceWebPopoutClaimListener(() => {
        releaseWorkspaceTabsBlockingWebPopout()
      }),
    []
  )

  return null
}
