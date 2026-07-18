"use client"

/**
 * Shared route selector for managed roots/drafts and forced-child display.
 * Unmanaged agents render nothing.
 */

import { useCallback, useState } from "react"
import { useTranslations } from "next-intl"
import { toast } from "sonner"
import { Route } from "lucide-react"

import {
  ContextMenuItem,
  ContextMenuRadioGroup,
  ContextMenuRadioItem,
  ContextMenuSeparator,
  ContextMenuSub,
  ContextMenuSubContent,
  ContextMenuSubTrigger,
} from "@/components/ui/context-menu"
import {
  setConversationDelegationRoute,
  setDraftDelegationRoutePreference,
} from "@/lib/api"
import { toErrorMessage } from "@/lib/app-error"
import type { AgentType, DelegationRoutePolicy } from "@/lib/types"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"

const MANAGED_AGENTS = new Set<AgentType>([
  "codex",
  "grok",
  "code_buddy",
  "claude_code",
])

export function isManagedRouteAgent(agentType: AgentType): boolean {
  return MANAGED_AGENTS.has(agentType)
}

export interface DelegationRouteMenuProps {
  agentType: AgentType
  conversationId: number | null
  parentId?: number | null
  connectionId?: string | null
  value: DelegationRoutePolicy | null
  onDraftChange?(value: DelegationRoutePolicy | null): void
  onPersistedChange?(value: DelegationRoutePolicy | null): void
}

export function DelegationRouteMenu({
  agentType,
  conversationId,
  parentId,
  connectionId,
  value,
  onDraftChange,
  onPersistedChange,
}: DelegationRouteMenuProps) {
  const t = useTranslations("Folder.chat.delegationRoute")
  const [busy, setBusy] = useState(false)
  const applyConversationUpsert = useAppWorkspaceStore(
    (s) => s.applyConversationUpsert
  )

  const isChild = parentId != null
  const managed = isManagedRouteAgent(agentType)
  const radioValue = value == null ? "inherit" : value

  const apply = useCallback(
    async (next: DelegationRoutePolicy | null) => {
      if (!managed || busy || isChild) return

      // Draft (no row yet): memory-only override.
      if (conversationId == null) {
        onDraftChange?.(next)
        if (connectionId) {
          try {
            await setDraftDelegationRoutePreference(connectionId, next)
          } catch (err) {
            toast.error(t("updateFailed"), {
              description: toErrorMessage(err),
            })
          }
        }
        return
      }

      setBusy(true)
      try {
        const summary = await setConversationDelegationRoute(
          conversationId,
          next
        )
        applyConversationUpsert(summary)
        onPersistedChange?.(next)
      } catch (err) {
        toast.error(t("updateFailed"), {
          description: toErrorMessage(err),
        })
      } finally {
        setBusy(false)
      }
    },
    [
      managed,
      busy,
      connectionId,
      conversationId,
      isChild,
      onDraftChange,
      onPersistedChange,
      t,
      applyConversationUpsert,
    ]
  )

  if (!managed) return null

  if (isChild) {
    return (
      <>
        <ContextMenuSeparator />
        <ContextMenuItem disabled data-disabled="">
          {t("codegInherited")}
        </ContextMenuItem>
      </>
    )
  }

  return (
    <>
      <ContextMenuSeparator />
      <ContextMenuSub>
        <ContextMenuSubTrigger>
          <Route className="h-4 w-4" />
          {t("menuLabel")}
        </ContextMenuSubTrigger>
        <ContextMenuSubContent>
          <ContextMenuRadioGroup
            value={radioValue}
            onValueChange={(v) => {
              if (v === "inherit") void apply(null)
              else if (v === "codeg" || v === "native") void apply(v)
            }}
          >
            <ContextMenuRadioItem value="inherit" disabled={busy}>
              {t("inheritGlobal")}
            </ContextMenuRadioItem>
            <ContextMenuRadioItem value="codeg" disabled={busy}>
              {t("codeg")}
            </ContextMenuRadioItem>
            <ContextMenuRadioItem value="native" disabled={busy}>
              {t("native")}
            </ContextMenuRadioItem>
          </ContextMenuRadioGroup>
        </ContextMenuSubContent>
      </ContextMenuSub>
    </>
  )
}
