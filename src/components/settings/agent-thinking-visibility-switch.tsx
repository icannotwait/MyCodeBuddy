"use client"

import { useState } from "react"
import { useTranslations } from "next-intl"
import { toast } from "sonner"
import { Switch } from "@/components/ui/switch"
import { acpUpdateAgentDisplayPreferences } from "@/lib/api"
import { toErrorMessage } from "@/lib/app-error"
import type { AgentType } from "@/lib/types"

export interface AgentThinkingVisibilitySwitchProps {
  agentType: AgentType
  checked: boolean
  onCheckedChange: (agentType: AgentType, checked: boolean) => void
}

export function AgentThinkingVisibilitySwitch({
  agentType,
  checked,
  onCheckedChange,
}: AgentThinkingVisibilitySwitchProps) {
  const t = useTranslations("AcpAgentSettings")
  const [saving, setSaving] = useState(false)

  const handleChange = async (next: boolean) => {
    if (saving) return
    onCheckedChange(agentType, next)
    setSaving(true)
    try {
      await acpUpdateAgentDisplayPreferences(agentType, next)
    } catch (error) {
      onCheckedChange(agentType, !next)
      toast.error(t("toasts.saveThinkingVisibilityFailed"), {
        description: toErrorMessage(error),
      })
    } finally {
      setSaving(false)
    }
  }

  return (
    <div className="mt-3 flex min-h-8 items-center justify-between gap-3 border-t pt-3">
      <label
        htmlFor={`show-thinking-${agentType}`}
        className="text-xs font-medium text-foreground"
      >
        {t("showThinking")}
      </label>
      <Switch
        id={`show-thinking-${agentType}`}
        checked={checked}
        disabled={saving}
        onCheckedChange={(next) => void handleChange(next)}
        aria-label={t("showThinking")}
      />
    </div>
  )
}
