"use client"

import { useTranslations } from "next-intl"

import { AGENT_LABELS, type AgentType } from "@/lib/types"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"

import { AgentConfigBody } from "./agent-config-body"
import { AGENT_TYPES, INHERIT, type AgentSpecForm } from "./serialization"

/** One single-agent stage (or the default): an agent picker plus its live
 *  mode/config. When `allowInherit`, the picker offers "use default" (`INHERIT`)
 *  and selecting it hides the config — the stage follows the default agent. */
export function StageAgentEditor({
  spec,
  onChange,
  allowInherit,
  disabled,
}: {
  spec: AgentSpecForm | typeof INHERIT
  onChange: (next: AgentSpecForm | typeof INHERIT) => void
  allowInherit: boolean
  disabled?: boolean
}) {
  const t = useTranslations("Loops.agentConfig")
  const isInherit = spec === INHERIT

  return (
    <div className="space-y-3">
      <Select
        value={isInherit ? INHERIT : spec.agent}
        onValueChange={(v) => {
          if (v === INHERIT) onChange(INHERIT)
          // Switching agent drops any prior mode/config (probed for the old one).
          else
            onChange({
              agent: v as AgentType,
              mode_id: null,
              config_values: {},
            })
        }}
        disabled={disabled}
      >
        <SelectTrigger className="h-8">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          {allowInherit && (
            <SelectItem value={INHERIT}>{t("useDefault")}</SelectItem>
          )}
          {AGENT_TYPES.map((a) => (
            <SelectItem key={a} value={a}>
              {AGENT_LABELS[a]}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>

      {isInherit ? (
        <p className="text-xs text-muted-foreground">{t("useDefaultHint")}</p>
      ) : (
        <AgentConfigBody
          agent={spec.agent}
          modeId={spec.mode_id}
          configValues={spec.config_values}
          onChange={({ mode_id, config_values }) =>
            onChange({ agent: spec.agent, mode_id, config_values })
          }
          disabled={disabled}
        />
      )}
    </div>
  )
}
