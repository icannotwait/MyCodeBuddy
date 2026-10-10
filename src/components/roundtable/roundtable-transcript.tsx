"use client"

import {
  createContext,
  useContext,
  useMemo,
  type ComponentProps,
  type ReactNode,
} from "react"
import { useLocale, useTranslations } from "next-intl"
import {
  ChevronRight,
  CircleCheck,
  CircleDot,
  Gavel,
  Lightbulb,
  LoaderCircle,
  MessagesSquare,
  Radio,
  Sparkles,
} from "lucide-react"
import { AgentIcon } from "@/components/agent-icon"
import { MessageResponse } from "@/components/ai-elements/message"
import { Badge } from "@/components/ui/badge"
import { redactRoundtableText } from "@/lib/roundtable/redact"
import type {
  RoundtableMessage,
  RoundtableProjection,
} from "@/lib/roundtable/types"
import {
  brandStyle,
  roundtableBrand,
  roundtableBrandKey,
  roundtableSeatBrands,
  type SeatBrand,
} from "@/lib/roundtable/brand"
import {
  buildRoundtableTranscript,
  replaceSeatAliases,
  type RoundtablePreviewInput,
  type TranscriptPhase,
  type TranscriptSpeaker,
  type TranscriptTurn,
} from "@/lib/roundtable/transcript"
import type { AgentType } from "@/lib/types"
import { cn } from "@/lib/utils"
import { RoundtableSafeContent } from "./roundtable-safe-content"

/** Static utility classes reading the `--rt-*` brand variables. */
export const BRAND_CLASSES = {
  ring: "ring-(--rt-c) dark:ring-(--rt-c-dark)",
  fill: "bg-(--rt-c)/10 dark:bg-(--rt-c-dark)/15",
  bar: "border-l-(--rt-c) dark:border-l-(--rt-c-dark)",
  border: "border-(--rt-c)/40 dark:border-(--rt-c-dark)/45",
  text: "text-(color:--rt-t) dark:text-(color:--rt-t-dark)",
} as const

const SeatBrandContext = createContext<Map<number, SeatBrand>>(new Map())

/** Brand for a speaker: its seat's brand, else derived from agent/model. */
export function speakerSeatBrand(
  speaker: Pick<
    TranscriptSpeaker,
    "seatOrdinal" | "agent" | "modelId" | "providerRef"
  >,
  seats: Map<number, SeatBrand>
): SeatBrand {
  const seat =
    speaker.seatOrdinal !== null ? seats.get(speaker.seatOrdinal) : undefined
  if (seat) return seat
  const key = roundtableBrandKey(speaker.agent, [
    speaker.modelId,
    speaker.providerRef,
  ])
  return { key, variant: 0, shared: false, brand: roundtableBrand(key) }
}

/**
 * Circular avatar: the agent mark centered on a brand-tinted disc with a
 * brand ring. When several members run the same agent, a seat number badge
 * keeps them apart (their rings are also shaded differently).
 */
export function RoundtableSpeakerAvatar({
  agent,
  seat,
  seatNumber,
  size = "md",
  className,
}: {
  agent: string
  seat: SeatBrand
  seatNumber?: number | null
  size?: "md" | "sm"
  className?: string
}) {
  return (
    <span
      aria-hidden="true"
      data-rt-avatar=""
      data-brand={seat.key}
      data-variant={seat.variant}
      style={brandStyle(seat.brand)}
      className={cn(
        "relative grid shrink-0 place-items-center rounded-full",
        BRAND_CLASSES.ring,
        BRAND_CLASSES.fill,
        size === "md" ? "size-8 ring-2" : "size-6 ring-[1.5px]",
        className
      )}
    >
      <AgentIcon
        agentType={agent as AgentType}
        className={cn(
          size === "md" ? "size-4.5" : "size-3.5",
          BRAND_CLASSES.text
        )}
      />
      {seat.shared && seatNumber != null ? (
        <span
          data-testid="seat-number"
          className={cn(
            "absolute -end-1 -bottom-1 grid size-3.5 place-items-center rounded-full bg-background text-[9px] leading-none font-semibold tabular-nums ring-1",
            BRAND_CLASSES.ring,
            BRAND_CLASSES.text
          )}
        >
          {seatNumber}
        </span>
      ) : null}
    </span>
  )
}

type Translate = ReturnType<typeof useTranslations<"Roundtable">>

/** Name shown for a speaker: the member seat, even when it moderates. */
function speakerName(speaker: TranscriptSpeaker, t: Translate) {
  if (speaker.seatOrdinal !== null)
    return `${t("member")} ${speaker.seatOrdinal + 1}`
  if (speaker.role === "moderator") return t("moderator")
  return t("unknown")
}

/** Inline label used where model output refers to a seat alias. */
function seatLabel(speaker: TranscriptSpeaker, t: Translate) {
  const name =
    speaker.role === "moderator"
      ? t("moderator")
      : speaker.ordinal !== null
        ? `${t("member")} ${speaker.ordinal + 1}`
        : t("unknown")
  return speaker.modelId ? `${name} · ${speaker.modelId}` : name
}

function escapeMarkdown(text: string) {
  return text.replace(/([\\`*_[\]<>#|~])/g, "\\$1")
}

const SeatLabelContext = createContext<(alias: string) => string | null>(
  () => null
)

export function RoundtableTranscript({
  projection,
  messages,
  previews,
  live,
  conclusionActions,
}: {
  projection: RoundtableProjection
  messages: RoundtableMessage[]
  previews?: Record<string, { preview: string | null }>
  /** Unverified, display-only live output of in-flight attempts. */
  live?: RoundtablePreviewInput[]
  /** Rendered under the accepted conclusion (save / download / copy). */
  conclusionActions?: ReactNode
}) {
  const t = useTranslations("Roundtable")
  const locale = useLocale()
  const previewInputs = useMemo(() => {
    const liveInputs = live ?? []
    const liveIds = new Set(liveInputs.map((item) => item.attemptId))
    return [
      ...Object.entries(previews ?? {}).flatMap(([attemptId, view]) =>
        view.preview === null || liveIds.has(attemptId)
          ? []
          : [{ attemptId, text: view.preview }]
      ),
      ...liveInputs,
    ]
  }, [previews, live])
  const model = useMemo(
    () => buildRoundtableTranscript(projection, messages, previewInputs),
    [projection, messages, previewInputs]
  )
  const seatBrands = useMemo(
    () => roundtableSeatBrands(projection.body.replay.config),
    [projection.body.replay.config]
  )
  const labelFor = useMemo(() => {
    const labels = new Map(
      Object.entries(model.seats).map(([alias, speaker]) => [
        alias,
        escapeMarkdown(seatLabel(speaker, t)),
      ])
    )
    return (alias: string) => labels.get(alias) ?? null
  }, [model.seats, t])

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
    <SeatBrandContext.Provider value={seatBrands}>
      <SeatLabelContext.Provider value={labelFor}>
        <section
          aria-label={t("results")}
          role="log"
          data-testid="roundtable-transcript"
          className="flex w-full max-w-3xl min-w-0 flex-col gap-5"
        >
          <h3 className="text-sm font-semibold">{t("results")}</h3>
          {model.phases.map((phase) => {
            const title = phaseTitle(phase)
            const conclusion = phase.kind === "synthesis"
            const Icon =
              phase.kind === "proposal"
                ? Lightbulb
                : phase.kind === "critique"
                  ? MessagesSquare
                  : conclusion
                    ? Sparkles
                    : CircleDot
            return (
              <section
                key={phase.id}
                aria-label={title}
                data-phase={phase.kind}
                className={cn(
                  "flex min-w-0 flex-col gap-1",
                  conclusion &&
                    "rounded-2xl border border-primary/30 bg-primary/5 p-3 shadow-xs sm:p-4"
                )}
              >
                <div className="flex min-w-0 items-center gap-3">
                  {conclusion ? null : (
                    <div className="h-px min-w-4 flex-1 bg-border" />
                  )}
                  <h4
                    className={cn(
                      "inline-flex shrink-0 items-center gap-1.5 text-xs font-semibold tracking-wide",
                      conclusion
                        ? "text-sm text-primary"
                        : "h-7 rounded-full border bg-background px-3 leading-none text-muted-foreground"
                    )}
                  >
                    <Icon aria-hidden="true" className="size-3.5 shrink-0" />
                    {title}
                  </h4>
                  <div className="h-px min-w-4 flex-1 bg-border" />
                </div>
                <ol className="min-w-0">
                  {phase.turns.map((turn, index) => (
                    <li key={turn.key} className="min-w-0 list-none">
                      <TranscriptTurnView
                        turn={turn}
                        locale={locale}
                        seats={model.seats}
                        last={index === phase.turns.length - 1}
                        conclusion={conclusion}
                      />
                    </li>
                  ))}
                </ol>
                {conclusion ? conclusionActions : null}
              </section>
            )
          })}
        </section>
      </SeatLabelContext.Provider>
    </SeatBrandContext.Provider>
  )
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
  const labelFor = useContext(SeatLabelContext)
  const visible = replaceSeatAliases(
    redactRoundtableText(text).trim(),
    labelFor
  )
  if (!visible) return null
  return (
    <MessageResponse className="max-w-full min-w-0 leading-relaxed [overflow-wrap:anywhere] [&_pre]:max-w-full [&_pre]:overflow-x-auto">
      {visible}
    </MessageResponse>
  )
}

function Chip({ children, className, ...props }: ComponentProps<"span">) {
  return (
    <span
      {...props}
      className={cn(
        "inline-flex h-5 items-center gap-1 rounded-full px-2 text-[11px] leading-none font-medium whitespace-nowrap",
        className
      )}
    >
      {children}
    </span>
  )
}

const STANCE_TONES: Record<string, string> = {
  support: "bg-emerald-500/10 text-emerald-700 dark:text-emerald-300",
  challenge: "bg-rose-500/10 text-rose-700 dark:text-rose-300",
  clarify: "bg-sky-500/10 text-sky-700 dark:text-sky-300",
  revise: "bg-amber-500/10 text-amber-700 dark:text-amber-300",
}

const AGREEMENT_TONES: Record<string, string> = {
  explicit_agreement:
    "bg-emerald-500/10 text-emerald-700 dark:text-emerald-300",
  compatible_positions: "bg-sky-500/10 text-sky-700 dark:text-sky-300",
  unresolved: "bg-amber-500/10 text-amber-700 dark:text-amber-300",
}

const MUTED_TONE = "bg-muted text-muted-foreground"

/** "normal" is the default; only call out a priority that differs. */
function notablePriority(value: string | null) {
  return value !== null && value !== "normal"
}

function TranscriptTurnView({
  turn,
  locale,
  seats,
  last,
  conclusion,
}: {
  turn: TranscriptTurn
  locale: string
  seats: Record<string, TranscriptSpeaker>
  last: boolean
  conclusion: boolean
}) {
  const t = useTranslations("Roundtable")
  const seatBrands = useContext(SeatBrandContext)
  const seat = speakerSeatBrand(turn.speaker, seatBrands)
  const name = speakerName(turn.speaker, t)
  const moderatorTurn = turn.speaker.role === "moderator"
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
  // The happy path (first attempt, accepted) stays in the details row.
  const attemptWorthShowing =
    turn.attemptState !== null &&
    (turn.attemptState !== "accepted" ||
      (turn.attemptNo !== null && turn.attemptNo > 1))

  return (
    <article
      dir="auto"
      data-message-id={turn.messageId ?? turn.key}
      data-visibility={turn.visibility}
      data-speaker-role={turn.speaker.role}
      data-seat={turn.speaker.seatOrdinal ?? undefined}
      data-brand={seat.key}
      style={brandStyle(seat.brand)}
      className="relative flex min-w-0 gap-2 pt-4 sm:gap-3"
    >
      <div className="flex shrink-0 flex-col items-center">
        <RoundtableSpeakerAvatar
          agent={turn.speaker.agent}
          seat={seat}
          seatNumber={
            turn.speaker.seatOrdinal !== null
              ? turn.speaker.seatOrdinal + 1
              : null
          }
        />
        {last ? null : (
          <div
            aria-hidden="true"
            data-testid="turn-connector"
            className="mt-2 -mb-4 w-px flex-1 bg-border"
          />
        )}
      </div>
      <div className="min-w-0 flex-1 space-y-2 pb-1">
        <header className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 pt-1 text-xs text-muted-foreground">
          <span
            data-testid="speaker-name"
            className={cn(
              "text-sm leading-5 font-semibold",
              BRAND_CLASSES.text
            )}
          >
            {name}
          </span>
          {moderatorTurn ? (
            <Chip className="gap-1 bg-primary/10 text-primary">
              <Gavel aria-hidden="true" className="size-3" />
              {t("moderator")}
            </Chip>
          ) : turn.speaker.participantRole ? (
            <Chip
              className={cn(
                MUTED_TONE,
                "h-auto min-h-5 py-0.5 whitespace-normal [overflow-wrap:anywhere]"
              )}
            >
              {turn.speaker.participantRole}
            </Chip>
          ) : null}
          {turn.speaker.modelId ? (
            <span className="font-mono text-[11px] leading-5 [overflow-wrap:anywhere]">
              {turn.speaker.modelId}
            </span>
          ) : null}
          {turn.finishedAt ? (
            <span className="inline-flex items-center gap-2 leading-5 whitespace-nowrap">
              <span aria-hidden="true">·</span>
              <time dateTime={turn.finishedAt} title={turn.finishedAt}>
                {formatStamp(turn.finishedAt, locale)}
              </time>
            </span>
          ) : null}
          <span className="ms-auto flex flex-wrap items-center gap-1">
            {turn.visibility === "published" ? (
              <Badge
                variant="outline"
                className="border-emerald-500/30 bg-emerald-500/5 text-emerald-700 dark:text-emerald-300"
              >
                <CircleCheck aria-hidden="true" />
                {t("published")}
              </Badge>
            ) : null}
            {turn.live ? (
              <Badge
                variant="outline"
                data-testid="live-badge"
                className="h-auto border-sky-500/40 bg-sky-500/10 whitespace-normal text-sky-800 dark:text-sky-200"
              >
                <Radio aria-hidden="true" />
                {t("liveBadge")}
              </Badge>
            ) : null}
            {turn.visibility === "staged" ? (
              <Badge
                variant="outline"
                className="h-auto border-amber-500/40 bg-amber-500/10 whitespace-normal text-amber-800 dark:text-amber-200"
              >
                {t("staged")}
              </Badge>
            ) : null}
            {turn.kind === "abstain" ? (
              <Badge variant="outline">{t("abstain")}</Badge>
            ) : null}
            {attemptWorthShowing ? (
              <Badge variant="outline">
                {turn.attemptNo !== null
                  ? t("attemptBadge", {
                      count: turn.attemptNo,
                      state: turn.attemptState!,
                    })
                  : turn.attemptState}
              </Badge>
            ) : null}
          </span>
        </header>
        <div
          data-testid="turn-body"
          className={cn(
            "min-w-0 rounded-xl border border-l-[3px] bg-card px-3 py-3 shadow-xs sm:px-4",
            BRAND_CLASSES.bar,
            conclusion && "bg-background"
          )}
        >
          {turn.visibility === "preview" && turn.live ? (
            <LiveTurnBody turn={turn} />
          ) : turn.visibility === "preview" ? (
            <RoundtableSafeContent text={turn.summary ?? ""} />
          ) : (
            <div className="min-w-0 space-y-4 text-sm">
              {turn.recommendation ? (
                <div className="min-w-0 rounded-lg border border-primary/30 bg-primary/5 px-3 py-2.5">
                  <h5 className="mb-1 flex items-center gap-1.5 text-xs font-semibold text-primary">
                    <Sparkles aria-hidden="true" className="size-3.5" />
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
              {turn.summary ? <RoundtableMarkdown text={turn.summary} /> : null}
              {turn.reason ? (
                <TextList title={t("reason")}>
                  <li className="min-w-0">
                    <RoundtableMarkdown text={turn.reason} />
                  </li>
                </TextList>
              ) : null}
              {turn.claims.length > 0 ? (
                <TextList title={t("claims")}>
                  {turn.claims.map((claim, index) => (
                    <li key={`${claim.text}:${index}`} className="min-w-0">
                      <RoundtableMarkdown text={claim.text} />
                      {claim.confidence ? (
                        <Chip className={cn(MUTED_TONE, "mt-1")}>
                          {confidence(claim.confidence)}
                        </Chip>
                      ) : null}
                    </li>
                  ))}
                </TextList>
              ) : null}
              {turn.responses.length > 0 ? (
                <TextList title={t("responses")}>
                  {turn.responses.map((response, index) => (
                    <li key={`${response.text}:${index}`} className="min-w-0">
                      {response.stance || notablePriority(response.priority) ? (
                        <p className="mb-1 flex flex-wrap gap-1">
                          {response.stance ? (
                            <Chip
                              className={
                                STANCE_TONES[response.stance] ?? MUTED_TONE
                              }
                            >
                              {stance(response.stance)}
                            </Chip>
                          ) : null}
                          {notablePriority(response.priority) ? (
                            <Chip
                              className={
                                response.priority === "critical"
                                  ? "bg-destructive/10 text-destructive"
                                  : MUTED_TONE
                              }
                            >
                              {priority(response.priority!)}
                            </Chip>
                          ) : null}
                        </p>
                      ) : null}
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
              {turn.consensus.length > 0 ? (
                <TextList title={t("consensus")}>
                  {turn.consensus.map((item, index) => {
                    const supporters = item.supporterAliases.flatMap(
                      (alias) => {
                        const seat = seats[alias]
                        return seat ? [{ alias, seat }] : []
                      }
                    )
                    return (
                      <li key={`${item.text}:${index}`} className="min-w-0">
                        {item.agreement ? (
                          <Chip
                            className={cn(
                              "mb-1",
                              AGREEMENT_TONES[item.agreement] ?? MUTED_TONE
                            )}
                          >
                            {agreement(item.agreement)}
                          </Chip>
                        ) : null}
                        <RoundtableMarkdown text={item.text} />
                        {supporters.length > 0 ? (
                          <p
                            data-testid="consensus-supporters"
                            className="mt-1 flex flex-wrap items-center gap-1 text-xs text-muted-foreground"
                          >
                            <span>{t("supportedBy")}</span>
                            {supporters.map(({ alias, seat: supporter }) => (
                              <Chip
                                key={alias}
                                data-brand={
                                  speakerSeatBrand(supporter, seatBrands).key
                                }
                                style={brandStyle(
                                  speakerSeatBrand(supporter, seatBrands).brand
                                )}
                                className={cn(
                                  "border bg-background",
                                  BRAND_CLASSES.border,
                                  BRAND_CLASSES.text
                                )}
                              >
                                {seatLabel(supporter, t)}
                              </Chip>
                            ))}
                          </p>
                        ) : null}
                        {item.inference ? (
                          <p className="text-xs text-muted-foreground">
                            {t("inference")}
                          </p>
                        ) : null}
                      </li>
                    )
                  })}
                </TextList>
              ) : null}
              <ConclusionList
                title={t("disagreements")}
                items={turn.disagreements}
                inferredLabel={t("inference")}
              />
              <ConclusionList
                title={t("alternatives")}
                items={turn.alternatives}
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
      </div>
    </article>
  )
}

function TextList({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="min-w-0 space-y-1.5">
      <h5 className="text-[11px] font-semibold tracking-wide text-muted-foreground uppercase">
        {title}
      </h5>
      <ul className="min-w-0 space-y-2 [&>li]:border-l-2 [&>li]:border-border/70 [&>li]:ps-3">
        {children}
      </ul>
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
    turn.speaker.providerRef ? [t("provider"), turn.speaker.providerRef] : null,
    turn.phaseId && !turn.phaseId.startsWith("kind:")
      ? [t("phaseId"), turn.phaseId]
      : null,
    turn.attemptId ? [t("attemptId"), turn.attemptId] : null,
    turn.attemptState
      ? [
          t("attempts"),
          turn.attemptNo !== null
            ? t("attemptBadge", {
                count: turn.attemptNo,
                state: turn.attemptState,
              })
            : turn.attemptState,
        ]
      : null,
    turn.publishedSeq ? [t("publishedSeq"), turn.publishedSeq] : null,
  ].filter((row): row is [string, string] => row !== null)
  if (
    rows.length === 0 &&
    turn.evidenceAliases.length === 0 &&
    turn.raw == null
  )
    return null
  return (
    <details className="group mt-3 min-w-0 border-t border-border/60 pt-2">
      <summary className="flex cursor-pointer list-none items-center gap-1.5 rounded-sm text-xs text-muted-foreground hover:text-foreground focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring [&::-webkit-details-marker]:hidden">
        <ChevronRight
          aria-hidden="true"
          className="size-3.5 shrink-0 transition-transform group-open:rotate-90"
        />
        {t("details")}
      </summary>
      <div className="mt-2 min-w-0 space-y-3 [overflow-wrap:anywhere]">
        {rows.length > 0 ? (
          <dl className="grid min-w-0 gap-1 text-xs text-muted-foreground sm:grid-cols-[max-content_1fr] sm:gap-x-4">
            {rows.map(([name, value]) => (
              <div key={name} className="contents">
                <dt className="font-medium">{name}</dt>
                <dd className="mb-1 font-mono break-all sm:mb-0">{value}</dd>
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

/**
 * Unverified live output of an in-flight attempt. Display only: it is replaced
 * by the verified message as soon as one claims the attempt.
 */
function LiveTurnBody({ turn }: { turn: TranscriptTurn }) {
  const t = useTranslations("Roundtable")
  const live = turn.live!
  const text = redactRoundtableText(turn.summary ?? "")
  const thought = redactRoundtableText(live.thought)
  return (
    <div
      data-testid="live-turn"
      aria-live="polite"
      aria-busy={!live.ended}
      className="min-w-0 space-y-2 text-sm"
    >
      <p className="text-xs text-muted-foreground">
        {live.ended ? t("liveEnded") : t("liveHint")}
      </p>
      {live.activity ? (
        <p
          data-testid="live-activity"
          className="flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground [overflow-wrap:anywhere]"
        >
          {live.ended ? null : (
            <LoaderCircle
              aria-hidden="true"
              className="size-3 shrink-0 animate-spin motion-reduce:animate-none"
            />
          )}
          {live.activity}
        </p>
      ) : null}
      {thought ? (
        <details
          data-testid="live-thinking"
          className="group min-w-0 rounded-lg border border-border/60 bg-muted/30 px-2.5 py-1.5"
        >
          <summary className="flex cursor-pointer list-none items-center gap-1.5 rounded-sm text-xs text-muted-foreground hover:text-foreground focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring [&::-webkit-details-marker]:hidden">
            <ChevronRight
              aria-hidden="true"
              className="size-3.5 shrink-0 transition-transform group-open:rotate-90"
            />
            {t("liveThinking")}
          </summary>
          <p className="mt-1.5 max-h-64 overflow-y-auto text-xs whitespace-pre-wrap text-muted-foreground [overflow-wrap:anywhere]">
            {live.thoughtOmitted ? `… ${thought}` : thought}
          </p>
        </details>
      ) : null}
      {text ? (
        <p
          dir="auto"
          data-testid="live-text"
          className="whitespace-pre-wrap [overflow-wrap:anywhere]"
        >
          {text}
          {live.truncated ? (
            <span className="block pt-1 text-xs text-muted-foreground">
              {t("liveTruncated")}
            </span>
          ) : null}
        </p>
      ) : live.ended ? null : (
        <p className="flex items-center gap-1.5 text-xs text-muted-foreground">
          <LoaderCircle
            aria-hidden="true"
            className="size-3 shrink-0 animate-spin motion-reduce:animate-none"
          />
          {t("liveWaiting")}
        </p>
      )}
    </div>
  )
}
