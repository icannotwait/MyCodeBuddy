"use client"

import { useRef, useState } from "react"
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
  const [savingByAgent, setSavingByAgent] = useState<
    Partial<Record<AgentType, boolean>>
  >({})
  const inflightRef = useRef<Partial<Record<AgentType, boolean>>>({})

  const saving = Boolean(savingByAgent[agentType])

  const handleChange = async (next: boolean) => {
    if (inflightRef.current[agentType]) return
    inflightRef.current[agentType] = true

    const requestAgent = agentType
    const previous = checked
    onCheckedChange(requestAgent, next)
    setSavingByAgent((prev) => ({ ...prev, [requestAgent]: true }))

    try {
      await acpUpdateAgentDisplayPreferences(requestAgent, next)
      // Re-apply in case a parent list refresh overwrote the optimistic value.
      onCheckedChange(requestAgent, next)
    } catch (error) {
      onCheckedChange(requestAgent, previous)
      toast.error(t("toasts.saveThinkingVisibilityFailed"), {
        description: toErrorMessage(error),
      })
    } finally {
      inflightRef.current[requestAgent] = false
      setSavingByAgent((prev) => ({ ...prev, [requestAgent]: false }))
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
