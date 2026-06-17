"use client"

import { type ReactNode } from "react"
import { useTranslations } from "next-intl"
import { Plus, X } from "lucide-react"

import type { LoopStage } from "@/lib/types"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Switch } from "@/components/ui/switch"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"

import { StageAgentEditor } from "@/components/loops/loop-config/stage-agent-editor"
import { ReviewersEditor } from "@/components/loops/loop-config/reviewers-editor"
import {
  INHERIT,
  ROUTE_AUTO,
  SINGLE_STAGES,
  type LoopConfigFormState,
} from "@/components/loops/loop-config/serialization"

// Re-export the form-state helpers/type so the issue-settings and space-defaults
// dialogs can keep importing them from here (the public entry point) even though
// they now live in the serialization module.
export {
  configToFormState,
  formStateToConfig,
  type LoopConfigFormState,
} from "@/components/loops/loop-config/serialization"

// Sub-tab trigger order inside the Agents tab (after "default"): the pipeline in
// execution order, with `review` sitting right after `implement` since review
// acts on implementation output. Drives only the tab order; the review sub-tab's
// body is the reviewers list, the rest are single-agent stages.
const STAGE_SUBTABS: LoopStage[] = [
  "triage",
  "refine",
  "design",
  "plan",
  "implement",
  "review",
  "finalize",
  "reflect",
]

/**
 * Tabbed editor for an `IssueConfig`. The "Agents" tab nests sub-tabs for the
 * default agent, each single-agent stage (with full mode/config, or "use
 * default"), and the review stage (reviewers list + pass rule). Validation and
 * Limits follow. Controlled — the host owns the `LoopConfigFormState` and
 * re-seeds it (e.g. on dialog open). `limitsExtra` lets a host append a field to
 * the Limits tab (the issue dialog uses it for the per-issue total budget, which
 * the space-defaults dialog has no concept of). Shared by both dialogs.
 */
export function LoopConfigForm({
  value,
  onChange,
  disabled,
  limitsExtra,
}: {
  value: LoopConfigFormState
  onChange: (next: LoopConfigFormState) => void
  disabled?: boolean
  limitsExtra?: ReactNode
}) {
  const t = useTranslations("Loops.issueSettings")
  const tStage = useTranslations("Loops.stage")
  const tRoute = useTranslations("Loops.route")
  const tCfg = useTranslations("Loops.config")

  const patch = (p: Partial<LoopConfigFormState>) =>
    onChange({ ...value, ...p })

  const setCommand = (i: number, next: string) => {
    const commands = [...value.validationCommands]
    commands[i] = next
    patch({ validationCommands: commands })
  }
  const addCommand = () =>
    patch({ validationCommands: [...value.validationCommands, ""] })
  const removeCommand = (i: number) =>
    patch({
      validationCommands: value.validationCommands.filter((_, j) => j !== i),
    })

  return (
    <Tabs defaultValue="agents" className="flex flex-col">
      <TabsList className="self-start">
        <TabsTrigger value="agents">{tCfg("tabAgents")}</TabsTrigger>
        <TabsTrigger value="validation">{tCfg("tabValidation")}</TabsTrigger>
        <TabsTrigger value="limits">{tCfg("tabLimits")}</TabsTrigger>
      </TabsList>

      <div className="mt-3 max-h-[52vh] overflow-y-auto pr-1">
        {/* Agents — nested sub-tabs: default + single stages + review */}
        <TabsContent value="agents" className="data-[state=inactive]:hidden">
          <Tabs
            defaultValue="default"
            orientation="vertical"
            className="flex gap-3"
          >
            <TabsList className="h-auto shrink-0 flex-col items-stretch">
              <TabsTrigger value="default">{tCfg("subtabDefault")}</TabsTrigger>
              {STAGE_SUBTABS.map((s) => (
                <TabsTrigger key={s} value={s}>
                  {tStage(s)}
                </TabsTrigger>
              ))}
            </TabsList>

            <div className="min-w-0 flex-1">
              <TabsContent
                value="default"
                className="space-y-2 data-[state=inactive]:hidden"
              >
                <Label>{t("defaultAgent")}</Label>
                <StageAgentEditor
                  spec={value.defaultSpec}
                  allowInherit={false}
                  onChange={(next) => {
                    if (next !== INHERIT) patch({ defaultSpec: next })
                  }}
                  disabled={disabled}
                />
              </TabsContent>

              {SINGLE_STAGES.map((s) => (
                <TabsContent
                  key={s}
                  value={s}
                  className="space-y-2 data-[state=inactive]:hidden"
                >
                  <StageAgentEditor
                    spec={value.stageSpecs[s]}
                    allowInherit
                    onChange={(next) =>
                      patch({
                        stageSpecs: { ...value.stageSpecs, [s]: next },
                      })
                    }
                    disabled={disabled}
                  />
                </TabsContent>
              ))}

              <TabsContent
                value="review"
                className="space-y-4 data-[state=inactive]:hidden"
              >
                <ReviewersEditor
                  value={value.reviewers}
                  onChange={(reviewers) => patch({ reviewers })}
                  disabled={disabled}
                />
                <div className="space-y-1.5">
                  <Label htmlFor="pass-rule">{t("reviewPassRule")}</Label>
                  <div id="pass-rule">
                    <Select
                      value={value.reviewPassRule}
                      onValueChange={(v) => patch({ reviewPassRule: v })}
                      disabled={disabled}
                    >
                      <SelectTrigger className="h-8">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="unanimous">
                          {t("ruleUnanimous")}
                        </SelectItem>
                        <SelectItem value="majority">
                          {t("ruleMajority")}
                        </SelectItem>
                      </SelectContent>
                    </Select>
                  </div>
                </div>
              </TabsContent>
            </div>
          </Tabs>
        </TabsContent>

        {/* Validation */}
        <TabsContent
          value="validation"
          className="space-y-2 data-[state=inactive]:hidden"
        >
          <Label>{t("validationCommands")}</Label>
          <p className="text-xs text-muted-foreground">{t("validationHint")}</p>
          <div className="space-y-2">
            {value.validationCommands.map((cmd, i) => (
              <div key={i} className="flex gap-2">
                <Input
                  value={cmd}
                  onChange={(e) => setCommand(i, e.target.value)}
                  placeholder={t("commandPlaceholder")}
                  className="h-8 font-mono text-xs"
                  disabled={disabled}
                />
                <Button
                  type="button"
                  variant="ghost"
                  size="icon"
                  className="h-8 w-8 shrink-0"
                  onClick={() => removeCommand(i)}
                  disabled={disabled}
                >
                  <X className="h-4 w-4" />
                </Button>
              </div>
            ))}
            <Button
              type="button"
              variant="outline"
              size="sm"
              className="h-8"
              onClick={addCommand}
              disabled={disabled}
            >
              <Plus className="mr-1 h-3.5 w-3.5" />
              {t("addCommand")}
            </Button>
          </div>
        </TabsContent>

        {/* Limits */}
        <TabsContent
          value="limits"
          className="space-y-4 data-[state=inactive]:hidden"
        >
          <div className="grid grid-cols-2 gap-3">
            <div className="space-y-1.5">
              <Label htmlFor="max-attempts">{t("maxAttempts")}</Label>
              <Input
                id="max-attempts"
                type="number"
                min={0}
                value={value.maxAttempts}
                onChange={(e) => patch({ maxAttempts: e.target.value })}
                className="h-8"
                disabled={disabled}
              />
              <p className="text-xs text-muted-foreground">
                {t("maxAttemptsHint")}
              </p>
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="force-route">{t("forceRoute")}</Label>
              <div id="force-route">
                <Select
                  value={value.forceRoute}
                  onValueChange={(v) => patch({ forceRoute: v })}
                  disabled={disabled}
                >
                  <SelectTrigger className="h-8">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value={ROUTE_AUTO}>{t("routeAuto")}</SelectItem>
                    <SelectItem value="full">{tRoute("full")}</SelectItem>
                    <SelectItem value="skip_design">
                      {tRoute("skip_design")}
                    </SelectItem>
                    <SelectItem value="direct">{tRoute("direct")}</SelectItem>
                  </SelectContent>
                </Select>
              </div>
            </div>
          </div>

          <div className="flex items-center justify-between gap-3">
            <div>
              <Label htmlFor="auto-merge">{t("autoMerge")}</Label>
              <p className="text-xs text-muted-foreground">
                {t("autoMergeHint")}
              </p>
            </div>
            <Switch
              id="auto-merge"
              checked={value.autoMerge}
              onCheckedChange={(v) => patch({ autoMerge: v })}
              disabled={disabled}
            />
          </div>

          <div className="grid grid-cols-2 gap-3">
            <div className="space-y-1.5">
              <Label htmlFor="iter-timeout">{t("iterationTimeout")}</Label>
              <Input
                id="iter-timeout"
                type="number"
                min={1}
                value={value.iterationTimeoutSecs}
                onChange={(e) =>
                  patch({ iterationTimeoutSecs: e.target.value })
                }
                placeholder={t("unlimitedPlaceholder")}
                className="h-8"
                disabled={disabled}
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="per-turn-budget">{t("tokenBudgetPerTurn")}</Label>
              <Input
                id="per-turn-budget"
                type="number"
                min={1}
                value={value.tokenBudgetPerTurn}
                onChange={(e) => patch({ tokenBudgetPerTurn: e.target.value })}
                placeholder={t("unlimitedPlaceholder")}
                className="h-8"
                disabled={disabled}
              />
            </div>
          </div>

          <div className="space-y-1.5">
            <Label htmlFor="stall-alert">{t("stallAlertSecs")}</Label>
            <Input
              id="stall-alert"
              type="number"
              min={1}
              value={value.stallAlertSecs}
              onChange={(e) => patch({ stallAlertSecs: e.target.value })}
              placeholder={t("offPlaceholder")}
              className="h-8"
              disabled={disabled}
            />
            <p className="text-xs text-muted-foreground">
              {t("stallAlertHint")}
            </p>
          </div>

          {limitsExtra}
        </TabsContent>
      </div>
    </Tabs>
  )
}
