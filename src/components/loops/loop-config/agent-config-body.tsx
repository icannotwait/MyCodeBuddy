"use client"

import { useTranslations } from "next-intl"
import { Loader2 } from "lucide-react"

import type { AgentType, SessionConfigOptionInfo } from "@/lib/types"
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"

import { DEFAULT_SENTINEL } from "./serialization"
import { useAgentOptions } from "./snapshot-cache"

/** Probes `agent` and renders its mode row (when standalone) + config option
 *  rows. Reused by each per-stage agent editor and by each reviewer row. */
export function AgentConfigBody({
  agent,
  modeId,
  configValues,
  onChange,
  disabled,
}: {
  agent: AgentType
  modeId: string | null
  configValues: Record<string, string>
  onChange: (next: {
    mode_id: string | null
    config_values: Record<string, string>
  }) => void
  disabled?: boolean
}) {
  const t = useTranslations("Loops.reviewers")
  const { snapshot, loading, error } = useAgentOptions(agent)

  const setMode = (modeId: string | null) =>
    onChange({ mode_id: modeId, config_values: configValues })
  const setConfigValue = (optionId: string, valueId: string | null) => {
    const next = { ...configValues }
    if (valueId === null) delete next[optionId]
    else next[optionId] = valueId
    onChange({ mode_id: modeId, config_values: next })
  }

  const hasModes =
    !!snapshot?.modes && snapshot.modes.available_modes.length > 0
  const hasOptions = !!snapshot && snapshot.config_options.length > 0
  // Mirror the chat input / delegation panel: when an agent exposes both modes
  // and config options, the mode is already one of the options — hide the
  // standalone mode row to avoid a duplicate.
  const showStandaloneMode = hasModes && !hasOptions

  return (
    <>
      {loading && (
        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          <Loader2 className="size-3.5 animate-spin" aria-hidden />
          {t("probing")}
        </div>
      )}
      {error && !loading && (
        <p className="text-xs text-muted-foreground">{t("probeFailed")}</p>
      )}
      {!loading && !error && snapshot && (
        <div className="space-y-2">
          {showStandaloneMode && snapshot.modes && (
            <AgentModeRow
              modes={snapshot.modes.available_modes}
              agentDefaultModeId={snapshot.modes.current_mode_id}
              overrideModeId={modeId}
              onChange={setMode}
              disabled={disabled}
            />
          )}
          {snapshot.config_options.map((option) => (
            <AgentConfigRow
              key={option.id}
              option={option}
              overrideValue={configValues[option.id] ?? null}
              onChange={(valueId) => setConfigValue(option.id, valueId)}
              disabled={disabled}
            />
          ))}
          {!showStandaloneMode && !hasOptions && (
            <p className="text-xs text-muted-foreground">{t("noConfig")}</p>
          )}
        </div>
      )}
    </>
  )
}

function AgentModeRow({
  modes,
  agentDefaultModeId,
  overrideModeId,
  onChange,
  disabled,
}: {
  modes: Array<{ id: string; name: string; description?: string | null }>
  agentDefaultModeId: string
  overrideModeId: string | null
  onChange: (modeId: string | null) => void
  disabled?: boolean
}) {
  const t = useTranslations("Loops.reviewers")
  const agentDefaultName =
    modes.find((m) => m.id === agentDefaultModeId)?.name ?? agentDefaultModeId
  const selectValue = overrideModeId ?? DEFAULT_SENTINEL
  return (
    <div className="flex items-start justify-between gap-3">
      <div className="min-w-0 space-y-0.5">
        <p className="text-sm font-medium">{t("modeLabel")}</p>
        <p className="text-xs text-muted-foreground">
          {t("agentDefaultHint", { value: agentDefaultName })}
        </p>
      </div>
      <Select
        value={selectValue}
        onValueChange={(v) => onChange(v === DEFAULT_SENTINEL ? null : v)}
        disabled={disabled}
      >
        <SelectTrigger size="sm" className="w-44 shrink-0">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value={DEFAULT_SENTINEL}>
            {t("defaultOption", { value: agentDefaultName })}
          </SelectItem>
          {modes.map((mode) => (
            <SelectItem key={mode.id} value={mode.id}>
              {mode.name}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </div>
  )
}

function AgentConfigRow({
  option,
  overrideValue,
  onChange,
  disabled,
}: {
  option: SessionConfigOptionInfo
  overrideValue: string | null
  onChange: (valueId: string | null) => void
  disabled?: boolean
}) {
  const t = useTranslations("Loops.reviewers")
  if (option.kind.type !== "select") return null

  const allOptions =
    option.kind.groups.length > 0
      ? option.kind.groups.flatMap((g) => g.options)
      : option.kind.options
  const agentDefault = option.kind.current_value
  const agentDefaultLabel =
    allOptions.find((o) => o.value === agentDefault)?.name ?? agentDefault
  const selectValue = overrideValue ?? DEFAULT_SENTINEL

  return (
    <div className="flex items-start justify-between gap-3">
      <div className="min-w-0 space-y-0.5">
        <p className="text-sm font-medium">{option.name}</p>
        <p className="text-xs text-muted-foreground">
          {t("agentDefaultHint", { value: agentDefaultLabel })}
        </p>
      </div>
      <Select
        value={selectValue}
        onValueChange={(v) => onChange(v === DEFAULT_SENTINEL ? null : v)}
        disabled={disabled}
      >
        <SelectTrigger size="sm" className="w-56 shrink-0">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value={DEFAULT_SENTINEL}>
            {t("defaultOption", { value: agentDefaultLabel })}
          </SelectItem>
          {option.kind.groups.length > 0
            ? option.kind.groups.map((group) => (
                <SelectGroup key={group.group}>
                  <SelectLabel>{group.name}</SelectLabel>
                  {group.options.map((item) => (
                    <SelectItem
                      key={`${group.group}-${item.value}`}
                      value={item.value}
                    >
                      {item.name}
                    </SelectItem>
                  ))}
                </SelectGroup>
              ))
            : option.kind.options.map((item) => (
                <SelectItem key={item.value} value={item.value}>
                  {item.name}
                </SelectItem>
              ))}
        </SelectContent>
      </Select>
    </div>
  )
}
