"use client"

import { useEffect, useState } from "react"
import { useTranslations } from "next-intl"
import { toast } from "sonner"
import { Loader2 } from "lucide-react"

import { setLoopSpaceDefaultConfig } from "@/lib/loops-api"
import { toErrorMessage } from "@/lib/app-error"
import { defaultIssueConfig } from "@/lib/loop-config"
import type { IssueConfig } from "@/lib/types"
import {
  LoopConfigForm,
  type LoopConfigFormState,
  configToFormState,
  formStateToConfig,
} from "@/components/loops/loop-config-form"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"

/**
 * Editor for a space's default `IssueConfig`. New issues inherit this (resolved
 * at read time) unless they override with a custom config; changing it
 * propagates live to every inheriting issue. Reuses the shared tabbed
 * {@link LoopConfigForm} without the per-issue inherit toggle or total budget.
 * Saving persists via `set_loop_space_default_config` (emits `loop://changed`);
 * "reset" clears the default (`null`) so inheritors fall back to the engine
 * default.
 */
export function SpaceDefaultsDialog({
  spaceId,
  current,
  open,
  onOpenChange,
}: {
  spaceId: number
  current: IssueConfig | null
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const t = useTranslations("Loops.spaceDefaults")
  const tCommon = useTranslations("Loops.common")
  const tToasts = useTranslations("Loops.toasts")

  const [form, setForm] = useState<LoopConfigFormState>(() =>
    configToFormState(current ?? defaultIssueConfig())
  )
  const [saving, setSaving] = useState(false)
  const [resetting, setResetting] = useState(false)
  const busy = saving || resetting

  useEffect(() => {
    if (open) setForm(configToFormState(current ?? defaultIssueConfig()))
  }, [open, current])

  const persist = async (
    config: IssueConfig | null,
    setBusy: (v: boolean) => void
  ) => {
    setBusy(true)
    try {
      await setLoopSpaceDefaultConfig(spaceId, config)
      toast.success(tToasts("spaceDefaultSaved"))
      onOpenChange(false)
    } catch (err) {
      toast.error(tToasts("actionFailed", { message: toErrorMessage(err) }))
    } finally {
      setBusy(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{t("title")}</DialogTitle>
          <DialogDescription>{t("description")}</DialogDescription>
        </DialogHeader>

        <div className="space-y-4">
          <LoopConfigForm value={form} onChange={setForm} disabled={busy} />
        </div>

        <DialogFooter className="gap-2 sm:justify-between">
          <Button
            type="button"
            variant="ghost"
            className="text-muted-foreground"
            onClick={() => persist(null, setResetting)}
            disabled={busy}
          >
            {resetting && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
            {t("resetToDefault")}
          </Button>
          <div className="flex gap-2">
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
              disabled={busy}
            >
              {tCommon("cancel")}
            </Button>
            <Button
              type="button"
              onClick={() => persist(formStateToConfig(form), setSaving)}
              disabled={busy}
            >
              {saving && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
              {t("save")}
            </Button>
          </div>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
