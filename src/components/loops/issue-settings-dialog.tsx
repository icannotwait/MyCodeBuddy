"use client"

import { useEffect, useState } from "react"
import { useTranslations } from "next-intl"
import { toast } from "sonner"
import { Loader2 } from "lucide-react"

import { updateLoopIssueConfig } from "@/lib/loops-api"
import { toErrorMessage } from "@/lib/app-error"
import { defaultIssueConfig } from "@/lib/loop-config"
import type { IssueConfig, LoopIssueDetail } from "@/lib/types"
import {
  LoopConfigForm,
  type LoopConfigFormState,
  configToFormState,
  formStateToConfig,
} from "@/components/loops/loop-config-form"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"

/** Empty / non-positive → null (unlimited); otherwise the floored integer. */
function parsePositiveOrNull(s: string): number | null {
  const n = Number(s.trim())
  return Number.isFinite(n) && n > 0 ? Math.floor(n) : null
}

function budgetField(n: number | null | undefined): string {
  return n == null ? "" : String(n)
}

/**
 * Editor for a single issue's config. The issue either inherits the space
 * default (read-only preview, resolved at read time by the engine) or uses a
 * custom `IssueConfig` edited through the shared tabbed {@link LoopConfigForm}.
 * The total token budget is per-issue and always editable. Saving persists via
 * `update_loop_issue_config`, which emits `loop://changed` so the detail view
 * refreshes; the engine reads config fresh each dispatch, so edits to a running
 * issue take effect from its next iteration (surfaced as a hint).
 */
export function IssueSettingsDialog({
  open,
  onOpenChange,
  issue,
  spaceDefaultConfig = null,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  issue: LoopIssueDetail
  spaceDefaultConfig?: IssueConfig | null
}) {
  const t = useTranslations("Loops.issueSettings")
  const tCommon = useTranslations("Loops.common")
  const tToasts = useTranslations("Loops.toasts")

  const [inherit, setInherit] = useState(issue.config_inherits)
  const [form, setForm] = useState<LoopConfigFormState>(() =>
    configToFormState(issue.config)
  )
  const [tokenBudget, setTokenBudget] = useState(() =>
    budgetField(issue.token_budget)
  )
  const [saving, setSaving] = useState(false)

  // Re-seed each time the dialog opens, so a cancel + reopen discards unsaved
  // edits and a config change elsewhere is reflected.
  useEffect(() => {
    if (open) {
      setInherit(issue.config_inherits)
      setForm(configToFormState(issue.config))
      setTokenBudget(budgetField(issue.token_budget))
    }
  }, [open, issue])

  // Read-only preview of what an inheriting issue resolves to: the space
  // default, or the engine default when the space has none.
  const inheritedForm = configToFormState(
    spaceDefaultConfig ?? defaultIssueConfig()
  )

  const onSave = async () => {
    setSaving(true)
    try {
      // On inherit the backend keeps the stored config as the last custom value
      // (it ignores this arg for the config column); on custom it writes it.
      await updateLoopIssueConfig(
        issue.id,
        formStateToConfig(form),
        parsePositiveOrNull(tokenBudget),
        inherit
      )
      toast.success(tToasts("configSaved"))
      onOpenChange(false)
    } catch (err) {
      toast.error(tToasts("actionFailed", { message: toErrorMessage(err) }))
    } finally {
      setSaving(false)
    }
  }

  const segBtn = (active: boolean) =>
    "flex-1 rounded-md px-3 py-1 text-xs font-medium transition-colors " +
    (active
      ? "bg-background text-foreground shadow-sm"
      : "text-muted-foreground hover:text-foreground")

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{t("title")}</DialogTitle>
          <DialogDescription>{t("description")}</DialogDescription>
        </DialogHeader>

        {issue.status === "running" && (
          <p className="rounded-md bg-muted px-3 py-2 text-xs text-muted-foreground">
            {t("runningHint")}
          </p>
        )}

        <div className="flex items-center gap-1 rounded-lg bg-muted p-1">
          <button
            type="button"
            className={segBtn(inherit)}
            onClick={() => setInherit(true)}
          >
            {t("useSpaceDefault")}
          </button>
          <button
            type="button"
            className={segBtn(!inherit)}
            onClick={() => setInherit(false)}
          >
            {t("custom")}
          </button>
        </div>
        {inherit && (
          <p className="text-xs text-muted-foreground">{t("inheritHint")}</p>
        )}

        <div className="space-y-4">
          <LoopConfigForm
            value={inherit ? inheritedForm : form}
            onChange={setForm}
            disabled={inherit}
          />

          <div className="space-y-1.5">
            <Label htmlFor="total-budget">{t("tokenBudget")}</Label>
            <Input
              id="total-budget"
              type="number"
              min={1}
              value={tokenBudget}
              onChange={(e) => setTokenBudget(e.target.value)}
              placeholder={t("unlimitedPlaceholder")}
              className="h-8"
            />
          </div>
        </div>

        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => onOpenChange(false)}
            disabled={saving}
          >
            {tCommon("cancel")}
          </Button>
          <Button type="button" onClick={onSave} disabled={saving}>
            {saving && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
            {t("save")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
