"use client"

/**
 * Read-only inline card for the codeg-mcp `ask_user_question` tool as it appears
 * in the message stream (historical transcripts + the in-flight tool marker).
 *
 * The live, interactive answering happens in `AskQuestionCard`, pinned below the
 * stream and driven by the `question_request` event — this card never collects
 * an answer. It mirrors that card's option layout but renders every choice as a
 * disabled card with the user's selection checked/highlighted, and drops the
 * footer actions. The Q&A is reconstructed from the tool's raw input JSON + the
 * companion's rendered result text (see `@/lib/ask-question`).
 */

import { useMemo } from "react"
import { useTranslations } from "next-intl"
import { Loader2, MessageCircleQuestionMark } from "lucide-react"

import { Badge } from "@/components/ui/badge"
import { Checkbox } from "@/components/ui/checkbox"
import { Label } from "@/components/ui/label"
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group"
import { cn } from "@/lib/utils"
import {
  parseAskQuestionInput,
  parseAskQuestionOutcome,
  splitRecommended,
  type AskQuestion,
} from "@/lib/ask-question"
import type { ToolCallState } from "@/lib/adapters/ai-elements-adapter"

// Separator for the composite "header + question" Map key. A control char (unit
// separator) that never appears in agent-authored text, so the join can't be
// ambiguous between two different (header, question) pairs.
const KEY_SEP = String.fromCharCode(31)

/** Single-select sentinel for a free-text "Other" answer, mirroring AskQuestionCard. */
const OTHER_VALUE = "__other__"

interface Props {
  input?: string | null
  output?: string | null
  errorText?: string | null
  state?: ToolCallState
}

export function AskQuestionResultCard({
  input,
  output,
  errorText,
  state,
}: Props) {
  const t = useTranslations("Folder.chat.askQuestionResult")

  const questions = useMemo(() => parseAskQuestionInput(input), [input])
  const outcome = useMemo(() => parseAskQuestionOutcome(output), [output])

  // Map each answered block back to its question by (header, question) text.
  // The backend emits answers in asked order but drops unanswered questions, so
  // a positional zip would misalign — keying on the text is robust to drops.
  const selectedByKey = useMemo(() => {
    const m = new Map<string, string[]>()
    for (const a of outcome?.answers ?? []) {
      m.set(`${a.header}${KEY_SEP}${a.question}`, a.selected)
    }
    return m
  }, [outcome])

  // Fall back to the answered blocks when the input JSON didn't parse (e.g. a
  // truncated historical transcript) so the card still shows what was asked.
  const displayQuestions = useMemo<AskQuestion[]>(() => {
    if (questions.length > 0) return questions
    return (outcome?.answers ?? []).map((a) => ({
      question: a.question,
      header: a.header,
      multiSelect: false,
      options: [],
    }))
  }, [questions, outcome])

  const isError = !!errorText?.trim()
  // Still blocking on the pinned interactive card: the tool is running and no
  // result text has arrived yet (`outcome` is null only for empty output).
  const isRunning = state === "input-available" || state === "input-streaming"
  const isInFlight = !isError && !outcome && isRunning
  const isDeclined = !!outcome?.declined

  const subtitle = isInFlight
    ? t("awaiting")
    : isDeclined
      ? t("declined")
      : null

  // A disabled option card. Selected stays full-opacity with the primary accent
  // so the choice stands out; the rest dim back like disabled controls.
  const cardClass = (selected: boolean) =>
    cn(
      "flex w-full items-start gap-2.5 rounded-lg border p-2.5 font-normal",
      selected ? "border-primary bg-primary/10" : "border-border/60 opacity-60"
    )

  // Label + recommended badge; the description is shown only for the chosen
  // option so the card stays compact and the answer reads at a glance.
  const optionBody = (
    text: string,
    recommended: boolean,
    description?: string
  ) => (
    <span className="min-w-0 flex-1">
      <span className="flex flex-wrap items-center gap-1.5 text-sm font-medium">
        {text}
        {recommended && (
          <Badge variant="secondary" className="text-[10px]">
            {t("recommended")}
          </Badge>
        )}
      </span>
      {description && (
        <span className="mt-0.5 block text-xs text-muted-foreground">
          {description}
        </span>
      )}
    </span>
  )

  const otherBody = (label: string) => (
    <span className="min-w-0 flex-1 text-sm font-medium">
      {label}
      <Badge variant="outline" className="ml-1.5 text-[10px]">
        {t("other")}
      </Badge>
    </span>
  )

  const renderOptions = (q: AskQuestion) => {
    const selected =
      selectedByKey.get(`${q.header}${KEY_SEP}${q.question}`) ?? []
    const selectedSet = new Set(selected)
    // Selected labels that aren't one of the offered options = free-text "Other".
    const otherCustoms = selected.filter(
      (s) => !q.options.some((o) => o.label === s)
    )

    // No option metadata (a pseudo-question rebuilt from the result text): show
    // the selected labels as chips, since there are no choices to disable.
    if (q.options.length === 0) {
      return (
        <div className="flex flex-wrap gap-1.5">
          {selected.length === 0 ? (
            <span className="text-xs text-muted-foreground">
              {t("noSelection")}
            </span>
          ) : (
            selected.map((label) => (
              <Badge key={label} className="text-xs">
                {splitRecommended(label).text}
              </Badge>
            ))
          )}
        </div>
      )
    }

    if (q.multiSelect) {
      return (
        <div className="space-y-1.5">
          {q.options.map((opt) => {
            const sel = selectedSet.has(opt.label)
            const { text, recommended } = splitRecommended(opt.label)
            return (
              <Label
                key={opt.label}
                data-selected={sel ? "true" : "false"}
                className={cardClass(sel)}
              >
                <Checkbox checked={sel} disabled className="mt-0.5" />
                {optionBody(
                  text,
                  recommended,
                  sel ? opt.description : undefined
                )}
              </Label>
            )
          })}
          {otherCustoms.map((label) => (
            <Label
              key={`other-${label}`}
              data-selected="true"
              className={cardClass(true)}
            >
              <Checkbox checked disabled className="mt-0.5" />
              {otherBody(label)}
            </Label>
          ))}
        </div>
      )
    }

    // Single-select: drive a disabled RadioGroup off the chosen option's index
    // (or the "Other" sentinel) so exactly one radio reads as filled.
    const selectedIdx = q.options.findIndex((o) => selectedSet.has(o.label))
    const otherCustom = otherCustoms[0]
    const value = otherCustom
      ? OTHER_VALUE
      : selectedIdx >= 0
        ? String(selectedIdx)
        : ""
    return (
      <RadioGroup value={value} disabled className="gap-1.5">
        {q.options.map((opt, i) => {
          const sel = selectedSet.has(opt.label)
          const { text, recommended } = splitRecommended(opt.label)
          return (
            <Label
              key={opt.label}
              data-selected={sel ? "true" : "false"}
              className={cardClass(sel)}
            >
              <RadioGroupItem
                value={String(i)}
                className="mt-0.5 data-[state=checked]:border-primary data-[state=checked]:bg-primary"
              />
              {optionBody(text, recommended, sel ? opt.description : undefined)}
            </Label>
          )
        })}
        {otherCustom && (
          <Label data-selected="true" className={cardClass(true)}>
            <RadioGroupItem
              value={OTHER_VALUE}
              className="mt-0.5 data-[state=checked]:border-primary data-[state=checked]:bg-primary"
            />
            {otherBody(otherCustom)}
          </Label>
        )}
      </RadioGroup>
    )
  }

  return (
    <div
      data-testid="ask-question-result-card"
      className={cn(
        "mb-2 overflow-hidden rounded-xl border bg-card",
        isError ? "border-destructive/30" : "border-primary/30"
      )}
    >
      <div className="flex flex-col gap-3 p-3">
        <div className="flex items-start gap-2.5">
          <span className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-muted text-primary">
            <MessageCircleQuestionMark className="size-4" />
          </span>
          <div className="min-w-0 flex-1">
            <p className="text-sm font-medium">{t("title")}</p>
            {subtitle && (
              <p className="text-xs text-muted-foreground">{subtitle}</p>
            )}
          </div>
          {isInFlight && (
            <Loader2 className="size-4 shrink-0 animate-spin text-muted-foreground" />
          )}
        </div>

        {isError && (
          <p className="whitespace-pre-wrap text-xs text-destructive">
            {errorText?.trim()}
          </p>
        )}

        {isInFlight
          ? displayQuestions.length > 0 && (
              <div className="flex flex-wrap gap-1.5">
                {displayQuestions.map((q, i) => (
                  <Badge key={i} variant="outline" className="text-[10px]">
                    {q.header || q.question}
                  </Badge>
                ))}
              </div>
            )
          : displayQuestions.length > 0 && (
              <div className="space-y-4">
                {displayQuestions.map((q, i) => (
                  <div key={i} className="space-y-2">
                    <div className="flex items-center gap-2">
                      <Badge variant="outline" className="shrink-0 text-[10px]">
                        {q.multiSelect ? t("multiple") : t("single")}
                      </Badge>
                      {q.question && (
                        <p className="text-sm text-foreground/90">
                          {q.question}
                        </p>
                      )}
                    </div>
                    {renderOptions(q)}
                  </div>
                ))}
              </div>
            )}
      </div>
    </div>
  )
}
