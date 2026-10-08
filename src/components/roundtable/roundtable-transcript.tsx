"use client"

import { useMemo, type ReactNode } from "react"
import { useLocale, useTranslations } from "next-intl"
import { ChevronRight } from "lucide-react"
import { AgentIcon } from "@/components/agent-icon"
import { MessageResponse } from "@/components/ai-elements/message"
import { Badge } from "@/components/ui/badge"
import { redactRoundtableText } from "@/lib/roundtable/redact"
import type {
  RoundtableMessage,
  RoundtableProjection,
} from "@/lib/roundtable/types"
import {
  buildRoundtableTranscript,
  type TranscriptPhase,
  type TranscriptSpeaker,
  type TranscriptTurn,
} from "@/lib/roundtable/transcript"
import type { AgentType } from "@/lib/types"
import { cn } from "@/lib/utils"
import { RoundtableSafeContent } from "./roundtable-safe-content"

export function RoundtableTranscript({
  projection,
  messages,
  previews,
}: {
  projection: RoundtableProjection
  messages: RoundtableMessage[]
  previews?: Record<string, { preview: string | null }>
}) {
  const t = useTranslations("Roundtable")
  const locale = useLocale()
  const previewInputs = useMemo(
    () =>
      Object.entries(previews ?? {}).flatMap(([attemptId, view]) =>
        view.preview === null ? [] : [{ attemptId, text: view.preview }]
      ),
    [previews]
  )
  const model = useMemo(
    () => buildRoundtableTranscript(projection, messages, previewInputs),
    [projection, messages, previewInputs]
  )

  const phaseTitle = (phase: TranscriptPhase) => {
    if (phase.kind === "synthesis") return t("conclusion")
    if (
      phase.kind === "critique" &&
      model.critiqueRoundCount > 1 &&
      phase.critiqueRound
    )
      return t("phaseCritiqueRound", { round: phase.critiqueRound })
    if (phase.titleKey === "abstain") return t("abstain")
    if (phase.titleKey === "phaseProposal") return t("phaseProposal")
    if (phase.titleKey === "phaseCritique") return t("phaseCritique")
    if (phase.titleKey === "phaseSynthesis") return t("phaseSynthesis")
    return t("phaseUnknown")
  }

  return (
    <section
      aria-label={t("results")}
      role="log"
      data-testid="roundtable-transcript"
      className="flex min-w-0 flex-col gap-6"
    >
      <h3 className="text-sm font-semibold">{t("results")}</h3>
      {model.phases.map((phase) => {
        const title = phaseTitle(phase)
        return (
          <section
            key={phase.id}
            aria-label={title}
            data-phase={phase.kind}
            className={cn(
              "flex min-w-0 flex-col gap-1",
              phase.kind === "synthesis" &&
                "rounded-xl border border-primary/30 bg-primary/5 p-3 sm:p-4"
            )}
          >
            <div className="flex min-w-0 items-center gap-3">
              <div className="h-px min-w-4 flex-1 bg-border" />
              <h4 className="text-center text-xs font-semibold tracking-wide text-muted-foreground">
                {title}
              </h4>
              <div className="h-px min-w-4 flex-1 bg-border" />
            </div>
            <ol className="min-w-0">
              {phase.turns.map((turn) => (
                <li key={turn.key} className="min-w-0 list-none">
                  <TranscriptTurnView turn={turn} locale={locale} />
                </li>
              ))}
            </ol>
          </section>
        )
      })}
    </section>
  )
}

function speakerLabel(
  speaker: TranscriptSpeaker,
  labels: { member: string; moderator: string; unknown: string }
) {
  if (speaker.role === "moderator") return labels.moderator
  if (speaker.role === "member" && speaker.ordinal !== null)
    return `${labels.member} ${speaker.ordinal + 1}`
  return labels.unknown
}

function formatStamp(value: string, locale: string) {
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return value
  return new Intl.DateTimeFormat(locale, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(date)
}

function RoundtableMarkdown({ text }: { text: string }) {
  const visible = redactRoundtableText(text).trim()
  if (!visible) return null
  return (
    <MessageResponse className="max-w-full min-w-0 [overflow-wrap:anywhere] [&_pre]:max-w-full [&_pre]:overflow-x-auto">
      {visible}
    </MessageResponse>
  )
}

function TranscriptTurnView({
  turn,
  locale,
}: {
  turn: TranscriptTurn
  locale: string
}) {
  const t = useTranslations("Roundtable")
  const label = speakerLabel(turn.speaker, {
    member: t("member"),
    moderator: t("moderator"),
    unknown: t("unknown"),
  })
  const confidence = (value: string) => {
    if (value === "low") return t("confidenceLow")
    if (value === "medium") return t("confidenceMedium")
    if (value === "high") return t("confidenceHigh")
    return value
  }
  const stance = (value: string) => {
    if (value === "support") return t("stanceSupport")
    if (value === "challenge") return t("stanceChallenge")
    if (value === "clarify") return t("stanceClarify")
    if (value === "revise") return t("stanceRevise")
    return value
  }
  const priority = (value: string) => {
    if (value === "normal") return t("priorityNormal")
    if (value === "critical") return t("priorityCritical")
    return value
  }
  const agreement = (value: string) => {
    if (value === "explicit_agreement") return t("agreementExplicit")
    if (value === "compatible_positions") return t("agreementCompatible")
    if (value === "unresolved") return t("agreementUnresolved")
    return value
  }

  return (
    <article
      dir="auto"
      data-message-id={turn.messageId ?? turn.key}
      data-visibility={turn.visibility}
      data-speaker-role={turn.speaker.role}
      className="flex min-w-0 gap-3 py-3"
    >
      <div
        className="mt-0.5 flex size-7 shrink-0 items-center justify-center rounded-full bg-secondary text-secondary-foreground"
        aria-hidden="true"
      >
        <AgentIcon
          agentType={turn.speaker.agent as AgentType}
          className="size-4"
        />
      </div>
      <div className="min-w-0 flex-1 space-y-2">
        <header className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
          <span className="text-sm font-medium">{label}</span>
          {turn.speaker.participantRole ? (
            <span className="text-xs text-muted-foreground [overflow-wrap:anywhere]">
              {turn.speaker.participantRole}
            </span>
          ) : null}
          {turn.speaker.modelId ? (
            <span className="text-xs text-muted-foreground [overflow-wrap:anywhere]">
              {turn.speaker.modelId}
            </span>
          ) : null}
          {turn.speaker.providerRef ? (
            <span className="text-xs text-muted-foreground [overflow-wrap:anywhere]">
              {turn.speaker.providerRef}
            </span>
          ) : null}
          {turn.finishedAt ? (
            <time
              dateTime={turn.finishedAt}
              className="text-xs text-muted-foreground"
            >
              {formatStamp(turn.finishedAt, locale)}
            </time>
          ) : null}
          {turn.visibility === "published" ? (
            <Badge variant="secondary">{t("published")}</Badge>
          ) : null}
          {turn.visibility === "staged" ? (
            <Badge variant="outline">{t("staged")}</Badge>
          ) : null}
          {turn.kind === "abstain" ? (
            <Badge variant="outline">{t("abstain")}</Badge>
          ) : null}
          {turn.attemptState ? (
            <Badge variant="outline">
              {turn.attemptNo !== null
                ? t("attemptBadge", {
                    count: turn.attemptNo,
                    state: turn.attemptState,
                  })
                : turn.attemptState}
            </Badge>
          ) : null}
        </header>
        {turn.visibility === "preview" ? (
          <RoundtableSafeContent text={turn.summary ?? ""} />
        ) : (
          <div className="min-w-0 space-y-3 text-sm">
            {turn.summary ? <RoundtableMarkdown text={turn.summary} /> : null}
            {turn.reason ? (
              <section className="min-w-0 space-y-1">
                <h5 className="text-xs font-medium text-muted-foreground">
                  {t("reason")}
                </h5>
                <RoundtableMarkdown text={turn.reason} />
              </section>
            ) : null}
            {turn.recommendation ? (
              <div className="min-w-0 rounded-lg border border-primary/30 bg-background px-3 py-2">
                <h5 className="mb-1 text-xs font-medium text-primary">
                  {t("recommendation")}
                </h5>
                <RoundtableMarkdown text={turn.recommendation.text} />
                {turn.recommendation.inference ? (
                  <p className="mt-1 text-xs text-muted-foreground">
                    {t("inference")}
                  </p>
                ) : null}
              </div>
            ) : null}
            {turn.claims.length > 0 ? (
              <TextList title={t("claims")}>
                {turn.claims.map((claim, index) => (
                  <li key={`${claim.text}:${index}`} className="min-w-0">
                    <RoundtableMarkdown text={claim.text} />
                    {claim.confidence ? (
                      <p className="text-xs text-muted-foreground">
                        {confidence(claim.confidence)}
                      </p>
                    ) : null}
                  </li>
                ))}
              </TextList>
            ) : null}
            {turn.responses.length > 0 ? (
              <TextList title={t("responses")}>
                {turn.responses.map((response, index) => (
                  <li key={`${response.text}:${index}`} className="min-w-0">
                    <p className="text-xs text-muted-foreground">
                      {[
                        response.stance ? stance(response.stance) : null,
                        response.priority ? priority(response.priority) : null,
                      ]
                        .filter(Boolean)
                        .join(" · ")}
                    </p>
                    <RoundtableMarkdown text={response.text} />
                  </li>
                ))}
              </TextList>
            ) : null}
            {turn.openQuestions.length > 0 ? (
              <TextList title={t("openQuestions")}>
                {turn.openQuestions.map((question, index) => (
                  <li key={`${question}:${index}`} className="min-w-0">
                    <RoundtableMarkdown text={question} />
                  </li>
                ))}
              </TextList>
            ) : null}
            {turn.positionChanges.length > 0 ? (
              <TextList title={t("positionChanges")}>
                {turn.positionChanges.map((change, index) => (
                  <li key={`${change.reason}:${index}`} className="min-w-0">
                    <RoundtableMarkdown text={change.reason} />
                  </li>
                ))}
              </TextList>
            ) : null}
            <ConclusionList
              title={t("alternatives")}
              items={turn.alternatives}
              inferredLabel={t("inference")}
            />
            {turn.consensus.length > 0 ? (
              <TextList title={t("consensus")}>
                {turn.consensus.map((item, index) => (
                  <li key={`${item.text}:${index}`} className="min-w-0">
                    {item.agreement ? (
                      <p className="text-xs text-muted-foreground">
                        {agreement(item.agreement)}
                      </p>
                    ) : null}
                    <RoundtableMarkdown text={item.text} />
                    {item.inference ? (
                      <p className="text-xs text-muted-foreground">
                        {t("inference")}
                      </p>
                    ) : null}
                  </li>
                ))}
              </TextList>
            ) : null}
            <ConclusionList
              title={t("disagreements")}
              items={turn.disagreements}
              inferredLabel={t("inference")}
            />
            <ConclusionList
              title={t("risks")}
              items={turn.risks}
              inferredLabel={t("inference")}
            />
            <ConclusionList
              title={t("decisionRequests")}
              items={turn.decisionRequests}
              inferredLabel={t("inference")}
            />
            {turn.coverage ? (
              <p className="text-xs text-muted-foreground">
                <span className="font-medium">{t("coverage")}: </span>
                {t("coverageCounts", {
                  succeeded: turn.coverage.succeeded,
                  absent: turn.coverage.absent,
                })}
              </p>
            ) : null}
          </div>
        )}
        <TurnDetails turn={turn} />
      </div>
    </article>
  )
}

function TextList({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="min-w-0 space-y-2">
      <h5 className="text-xs font-medium text-muted-foreground">{title}</h5>
      <ul className="min-w-0 space-y-2">{children}</ul>
    </section>
  )
}

function ConclusionList({
  title,
  items,
  inferredLabel,
}: {
  title: string
  items: { text: string; inference: boolean }[]
  inferredLabel: string
}) {
  if (items.length === 0) return null
  return (
    <TextList title={title}>
      {items.map((item, index) => (
        <li key={`${item.text}:${index}`} className="min-w-0">
          <RoundtableMarkdown text={item.text} />
          {item.inference ? (
            <p className="text-xs text-muted-foreground">{inferredLabel}</p>
          ) : null}
        </li>
      ))}
    </TextList>
  )
}

function TurnDetails({ turn }: { turn: TranscriptTurn }) {
  const t = useTranslations("Roundtable")
  const rows = [
    turn.messageId ? [t("messageId"), turn.messageId] : null,
    turn.bodyHash ? [t("bodyHash"), turn.bodyHash] : null,
    turn.speaker.speakerId ? [t("speakerId"), turn.speaker.speakerId] : null,
    turn.phaseId && !turn.phaseId.startsWith("kind:")
      ? [t("phaseId"), turn.phaseId]
      : null,
    turn.attemptId ? [t("attemptId"), turn.attemptId] : null,
    turn.publishedSeq ? [t("publishedSeq"), turn.publishedSeq] : null,
  ].filter((row): row is [string, string] => row !== null)
  if (
    rows.length === 0 &&
    turn.evidenceAliases.length === 0 &&
    turn.raw == null
  )
    return null
  return (
    <details className="group min-w-0 border-t border-border/60 pt-2">
      <summary className="flex cursor-pointer list-none items-center gap-1.5 rounded-sm text-xs text-muted-foreground focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring [&::-webkit-details-marker]:hidden">
        <ChevronRight
          aria-hidden="true"
          className="size-3.5 shrink-0 transition-transform group-open:rotate-90"
        />
        {t("details")}
      </summary>
      <div className="mt-2 min-w-0 space-y-3 [overflow-wrap:anywhere]">
        {rows.length > 0 ? (
          <dl className="grid min-w-0 gap-1 text-xs text-muted-foreground">
            {rows.map(([name, value]) => (
              <div key={name} className="min-w-0">
                <dt className="font-medium">{name}</dt>
                <dd className="break-all">{value}</dd>
              </div>
            ))}
          </dl>
        ) : null}
        {turn.evidenceAliases.length > 0 ? (
          <div className="min-w-0">
            <h5 className="text-xs font-medium text-muted-foreground">
              {t("evidenceAliases")}
            </h5>
            <ul className="mt-1 space-y-1 text-xs text-muted-foreground">
              {turn.evidenceAliases.map((alias) => (
                <li key={`${alias.kind}:${alias.alias}`} className="break-all">
                  {alias.kind}: {alias.alias}
                </li>
              ))}
            </ul>
          </div>
        ) : null}
        {turn.raw != null ? (
          <RoundtableSafeContent
            text={JSON.stringify(turn.raw, null, 2)}
            preview={false}
          />
        ) : null}
      </div>
    </details>
  )
}
