"use client"

import { useEffect, useState } from "react"
import { useTranslations } from "next-intl"
import { Loader2, RotateCcw } from "lucide-react"

import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"
import {
  resolveCompletionDecision,
  resolveDesignSelfReview,
  retryCompletionArtifact,
} from "@/lib/api"
import { toErrorMessage } from "@/lib/app-error"
import type { CompletionOutcome, CompletionProjectionV2 } from "@/lib/types"

type CompletionDecisionCardProps = {
  request: CompletionProjectionV2
  className?: string
  onResolved?: (completion: CompletionProjectionV2) => void
}

const REVIEWER_OUTCOMES: CompletionOutcome[] = [
  "approve",
  "approve_with_minors",
  "request_changes",
  "block",
]
const PRODUCER_OUTCOMES: CompletionOutcome[] = [
  "done",
  "done_with_concerns",
  "blocked",
]

export function CompletionDecisionCard({
  request,
  className,
  onResolved,
}: CompletionDecisionCardProps) {
  const t = useTranslations("Folder.chat.workflowGraph")
  const [current, setCurrent] = useState(request)
  const [selected, setSelected] = useState<CompletionOutcome | null>(null)
  const [pending, setPending] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    setCurrent(request)
    setSelected(null)
    setError(null)
  }, [request])

  const card = current.card
  const cas = card.attention
  const artifactRecovery = cas?.kind === "completion_artifact_recovery"
  const outcomes =
    card.role === "reviewer"
      ? REVIEWER_OUTCOMES
      : card.role === "author" ||
          card.role === "implementer" ||
          card.role === "fixer"
        ? PRODUCER_OUTCOMES
        : []
  const completionErrorMessage = (message: string): string => {
    if (message.includes("completion_decision_superseded")) {
      return t("completionStale")
    }
    if (message.includes("completion_decision_conflict")) {
      return t("completionConflict")
    }
    return message
  }

  const applyResult = (
    completion: CompletionProjectionV2 | null | undefined
  ) => {
    if (!completion) return
    setCurrent(completion)
    onResolved?.(completion)
  }

  const submit = async (outcome: CompletionOutcome) => {
    if (!cas || pending) return
    setSelected(outcome)
    setError(null)
    setPending(true)
    try {
      const result =
        cas.kind === "design_self_review_decision"
          ? await resolveDesignSelfReview({ cas, outcome })
          : await resolveCompletionDecision({ cas, outcome })
      applyResult(result.completion)
    } catch (cause: unknown) {
      setError(completionErrorMessage(toErrorMessage(cause)))
    } finally {
      setPending(false)
    }
  }

  const retryArtifact = async () => {
    if (!cas || !artifactRecovery || pending) return
    setError(null)
    setPending(true)
    try {
      const result = await retryCompletionArtifact({ cas })
      applyResult(result.completion)
    } catch (cause: unknown) {
      setError(completionErrorMessage(toErrorMessage(cause)))
    } finally {
      setPending(false)
    }
  }

  return (
    <div
      className={cn("space-y-2 border-t border-border/60 pt-2", className)}
      data-testid="completion-decision-card"
    >
      <div className="flex items-center justify-between gap-2">
        <span className="text-xs font-medium">
          {t(
            card.state === "resolved"
              ? "completionResolved"
              : card.state === "needs_decision"
                ? "completionNeedsDecision"
                : "completionBlocked"
          )}
        </span>
        {card.evidence_validated && (
          <span className="text-[10px] text-muted-foreground">
            {t("completionEvidenceValidated")}
          </span>
        )}
      </div>

      {card.summary && (
        <p className="break-words text-xs leading-5 text-muted-foreground">
          {card.summary}
        </p>
      )}
      {(card.source || card.report_file) && (
        <div className="flex flex-wrap gap-x-3 text-[10px] text-muted-foreground">
          {card.source && <span>{t(`completionSource.${card.source}`)}</span>}
          {card.report_file && (
            <span className="break-all">
              {t("completionReportFile", { file: card.report_file })}
            </span>
          )}
        </div>
      )}

      {card.state === "needs_decision" && cas && !artifactRecovery && (
        <div className="flex flex-wrap gap-1.5">
          {outcomes.map((outcome) => (
            <Button
              key={outcome}
              type="button"
              size="sm"
              variant={selected === outcome ? "secondary" : "outline"}
              aria-pressed={selected === outcome}
              disabled={pending}
              onClick={() => void submit(outcome)}
            >
              {pending && selected === outcome && (
                <Loader2 className="size-3.5 animate-spin" aria-hidden />
              )}
              {t(`completionOutcome.${outcome}`)}
            </Button>
          ))}
        </div>
      )}

      {artifactRecovery && cas && (
        <Button
          type="button"
          size="sm"
          variant="outline"
          disabled={pending}
          onClick={() => void retryArtifact()}
        >
          {pending ? (
            <Loader2 className="size-3.5 animate-spin" aria-hidden />
          ) : (
            <RotateCcw className="size-3.5" aria-hidden />
          )}
          {t("completionRetryArtifact")}
        </Button>
      )}

      {error && (
        <p role="alert" className="text-xs text-destructive">
          {error}
        </p>
      )}
    </div>
  )
}
