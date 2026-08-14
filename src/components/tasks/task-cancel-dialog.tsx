"use client"

import { useEffect, useState } from "react"
import { useTranslations } from "next-intl"
import { toast } from "sonner"
import { workTaskCancel } from "@/lib/api"
import { toErrorMessage } from "@/lib/app-error"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Label } from "@/components/ui/label"
import { Textarea } from "@/components/ui/textarea"
import type { WorkTask } from "@/lib/types"

interface TaskCancelDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  task: WorkTask | null
}

/**
 * Stop a task, on the record. A cancel almost always has a reason the board
 * cannot infer — wrong approach, changed requirements, started by mistake —
 * and that is exactly what the person requeuing it weeks later wants to read.
 * The reason rides the `canceled` entry of the progress timeline; it is a note
 * for humans and never reaches a later run's prompt (a requeue carries its own
 * note for that).
 *
 * Optional on purpose: an unexplained stop is still a legitimate stop, so the
 * confirm never waits on the textarea. Reached from the board card's cancel
 * button, and from the drawer's cancel / abandon — one backend transition, one
 * dialog.
 */
export function TaskCancelDialog({
  open,
  onOpenChange,
  task,
}: TaskCancelDialogProps) {
  const t = useTranslations("Tasks")
  const [reason, setReason] = useState("")
  const [submitting, setSubmitting] = useState(false)

  useEffect(() => {
    if (!open) return
    // A fresh box per open — a reason belongs to one cancel, not to the next.
    /* eslint-disable react-hooks/set-state-in-effect */
    setReason("")
    setSubmitting(false)
    /* eslint-enable react-hooks/set-state-in-effect */
  }, [open, task])

  const submit = async () => {
    if (!task) return
    setSubmitting(true)
    try {
      await workTaskCancel(task.id, reason.trim() || null)
      onOpenChange(false)
    } catch (e) {
      toast.error(toErrorMessage(e))
      setSubmitting(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-[28rem]">
        <DialogHeader>
          <DialogTitle>{t("cancelTitle")}</DialogTitle>
          <DialogDescription>{t("cancelDescription")}</DialogDescription>
        </DialogHeader>

        <div className="flex flex-col gap-1.5">
          <Label htmlFor="task-cancel-reason">{t("cancelReasonLabel")}</Label>
          <Textarea
            id="task-cancel-reason"
            value={reason}
            onChange={(e) => setReason(e.target.value)}
            placeholder={t("cancelReasonPlaceholder")}
            rows={3}
            autoFocus
          />
        </div>

        <DialogFooter>
          <Button
            type="button"
            variant="ghost"
            onClick={() => onOpenChange(false)}
            disabled={submitting}
          >
            {t("cancelKeep")}
          </Button>
          <Button type="button" onClick={submit} disabled={submitting}>
            {t("cancelSubmit")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
