"use client"

import { useTranslations } from "next-intl"
import { Plus, X } from "lucide-react"

import { AGENT_LABELS, type AgentType } from "@/lib/types"
import { Button } from "@/components/ui/button"
import { Label } from "@/components/ui/label"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"

import { AgentConfigBody } from "./agent-config-body"
import {
  AGENT_TYPES,
  INHERIT,
  isInheritReviewer,
  type ReviewerForm,
} from "./serialization"

export function ReviewersEditor({
  value,
  onChange,
  disabled,
}: {
  value: ReviewerForm[]
  onChange: (reviewers: ReviewerForm[]) => void
  disabled?: boolean
}) {
  const t = useTranslations("Loops.reviewers")

  const setRow = (i: number, next: ReviewerForm) =>
    onChange(value.map((r, j) => (j === i ? next : r)))
  const addRow = () =>
    onChange([
      ...value,
      { agent: "claude_code", mode_id: null, config_values: {} },
    ])
  const removeRow = (i: number) => onChange(value.filter((_, j) => j !== i))

  return (
    <div className="space-y-2">
      <Label>{t("heading")}</Label>
      <p className="text-xs text-muted-foreground">
        {value.length === 0 ? t("empty") : t("hint")}
      </p>
      <div className="space-y-2">
        {value.map((spec, i) => (
          <ReviewerRow
            key={i}
            index={i}
            spec={spec}
            onChange={(next) => setRow(i, next)}
            onRemove={() => removeRow(i)}
            disabled={disabled}
          />
        ))}
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="h-8"
          onClick={addRow}
          disabled={disabled}
        >
          <Plus className="mr-1 h-3.5 w-3.5" />
          {t("add")}
        </Button>
      </div>
    </div>
  )
}

function ReviewerRow({
  index,
  spec,
  onChange,
  onRemove,
  disabled,
}: {
  index: number
  spec: ReviewerForm
  onChange: (next: ReviewerForm) => void
  onRemove: () => void
  disabled?: boolean
}) {
  const t = useTranslations("Loops.reviewers")
  const tAgent = useTranslations("Loops.agentConfig")
  const inherit = isInheritReviewer(spec)

  return (
    <div className="space-y-2 rounded-md border bg-card/50 p-2.5">
      <div className="flex items-center gap-2">
        <span className="text-xs font-medium text-muted-foreground">
          {t("rowLabel", { n: index + 1 })}
        </span>
        <div className="flex-1">
          <Select
            value={inherit ? INHERIT : spec.agent}
            onValueChange={(v) =>
              // Switching agent drops any prior mode/config (they were probed
              // for the old agent and won't apply to the new one); INHERIT defers
              // this reviewer to the issue's default review agent.
              onChange(
                v === INHERIT
                  ? { inherit: true }
                  : { agent: v as AgentType, mode_id: null, config_values: {} }
              )
            }
            disabled={disabled}
          >
            <SelectTrigger className="h-8">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={INHERIT}>{tAgent("useDefault")}</SelectItem>
              {AGENT_TYPES.map((a) => (
                <SelectItem key={a} value={a}>
                  {AGENT_LABELS[a]}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <Button
          type="button"
          variant="ghost"
          size="icon"
          className="h-8 w-8 shrink-0"
          onClick={onRemove}
          disabled={disabled}
          aria-label={t("remove")}
        >
          <X className="h-4 w-4" />
        </Button>
      </div>

      {inherit ? (
        <p className="text-xs text-muted-foreground">{t("useDefaultHint")}</p>
      ) : (
        <AgentConfigBody
          agent={spec.agent}
          modeId={spec.mode_id ?? null}
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
