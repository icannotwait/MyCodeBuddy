"use client"

import { useRef } from "react"
import { useTranslations } from "next-intl"
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog"
import type { PendingTerminalClose } from "@/contexts/terminal-close-guard"

interface TerminalCloseConfirmDialogProps {
  pending: PendingTerminalClose | null
  onConfirm: () => void
  onCancel: () => void
}

export function TerminalCloseConfirmDialog({
  pending,
  onConfirm,
  onCancel,
}: TerminalCloseConfirmDialogProps) {
  const t = useTranslations("Folder.terminal")
  // AlertDialogAction closes the dialog and fires onOpenChange(false). Skip
  // onCancel for that path so confirm is not treated as dismiss.
  const confirmedRef = useRef(false)

  return (
    <AlertDialog
      open={pending !== null}
      onOpenChange={(open) => {
        if (!open) {
          if (confirmedRef.current) {
            confirmedRef.current = false
            return
          }
          onCancel()
        }
      }}
    >
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{t("confirmCloseTitle")}</AlertDialogTitle>
          <AlertDialogDescription>
            {pending?.kind === "one"
              ? t("confirmCloseRunning", { title: pending.title })
              : pending
                ? t("confirmCloseRunningCount", { count: pending.liveCount })
                : null}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>{t("confirmCloseCancel")}</AlertDialogCancel>
          <AlertDialogAction
            variant="destructive"
            onClick={() => {
              confirmedRef.current = true
              onConfirm()
            }}
          >
            {t("confirmCloseAction")}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}
