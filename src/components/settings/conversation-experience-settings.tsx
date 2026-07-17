"use client"

/**
 * Conversation-experience settings: automatic title agent selection.
 * Mounted under General settings before multi-agent delegation.
 */

import { useCallback, useMemo, useState } from "react"
import { useTranslations } from "next-intl"
import { Loader2, Sparkles } from "lucide-react"
import { toast } from "sonner"

import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { useAcpAgents } from "@/hooks/use-acp-agents"
import { toErrorMessage } from "@/lib/app-error"
import type { AgentType } from "@/lib/types"
import {
  useConversationExperienceBootstrap,
  useConversationExperienceStore,
} from "@/stores/conversation-experience-store"

const OFF_VALUE = "__off__"

export function ConversationExperienceSettingsSection() {
  const t = useTranslations("GeneralSettings")
  useConversationExperienceBootstrap()
  const settings = useConversationExperienceStore((s) => s.settings)
  const loading = useConversationExperienceStore((s) => s.loading)
  const setAutoTitleAgent = useConversationExperienceStore(
    (s) => s.setAutoTitleAgent
  )
  const { agents } = useAcpAgents()
  const [saving, setSaving] = useState(false)

  const savedAgent = settings?.auto_title_agent ?? null

  const choices = useMemo(() => {
    const enabledAvailable = agents.filter((a) => a.enabled && a.available)
    const savedUnavailable =
      savedAgent != null &&
      !enabledAvailable.some((a) => a.agent_type === savedAgent)
        ? (agents.find((a) => a.agent_type === savedAgent) ?? {
            agent_type: savedAgent,
            name: savedAgent,
            enabled: false,
            available: false,
          })
        : null
    return { enabledAvailable, savedUnavailable }
  }, [agents, savedAgent])

  const selectValue = savedAgent ?? OFF_VALUE

  const onChange = useCallback(
    async (value: string) => {
      const next: AgentType | null =
        value === OFF_VALUE ? null : (value as AgentType)
      setSaving(true)
      try {
        await setAutoTitleAgent(next)
      } catch (err: unknown) {
        toast.error(t("autoTitleSaveFailed", { message: toErrorMessage(err) }))
      } finally {
        setSaving(false)
      }
    },
    [setAutoTitleAgent, t]
  )

  return (
    <section className="rounded-xl border bg-card p-4 space-y-4">
      <div className="flex items-center gap-2">
        <Sparkles className="h-4 w-4 text-muted-foreground" aria-hidden />
        <h2 className="text-sm font-semibold">
          {t("conversationExperienceTitle")}
        </h2>
      </div>
      <p className="text-xs text-muted-foreground leading-5">
        {t("conversationExperienceDescription")}
      </p>

      {loading && !settings ? (
        <p className="flex items-center gap-2 text-xs text-muted-foreground">
          <Loader2 className="h-3.5 w-3.5 animate-spin" />
          {t("autoTitleLoading")}
        </p>
      ) : (
        <div className="flex items-center justify-between gap-3">
          <div className="space-y-1 min-w-0">
            <label htmlFor="auto-title-agent" className="text-sm font-medium">
              {t("autoTitleAgent")}
            </label>
          </div>
          <Select
            value={selectValue}
            onValueChange={onChange}
            disabled={saving || loading}
          >
            <SelectTrigger id="auto-title-agent" className="w-[220px] shrink-0">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={OFF_VALUE}>{t("autoTitleOff")}</SelectItem>
              {choices.enabledAvailable.map((agent) => (
                <SelectItem key={agent.agent_type} value={agent.agent_type}>
                  {agent.name}
                </SelectItem>
              ))}
              {choices.savedUnavailable && (
                <SelectItem
                  value={choices.savedUnavailable.agent_type}
                  disabled
                >
                  {t("autoTitleUnavailable", {
                    agent: choices.savedUnavailable.name,
                  })}
                </SelectItem>
              )}
            </SelectContent>
          </Select>
        </div>
      )}
    </section>
  )
}
