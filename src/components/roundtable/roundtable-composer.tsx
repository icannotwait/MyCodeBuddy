"use client"

import { useId, useState, type ReactNode } from "react"
import { useTranslations } from "next-intl"
import {
  CircleAlert,
  Clock,
  FileText,
  Gavel,
  Loader2,
  Plus,
  RefreshCw,
  X,
} from "lucide-react"
import { AgentIcon } from "@/components/agent-icon"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import {
  brandStyle,
  roundtableBrand,
  roundtableBrandKey,
  type SeatBrand,
} from "@/lib/roundtable/brand"
import {
  agentStatusReasonKey,
  errorReason,
  isAgentSelectable,
  seatProfiles,
  namedSeat,
  sortRoundtableAgents,
  type RoundtableAgentStatus,
} from "@/lib/roundtable/agents"
import type { ModelProviderInfo } from "@/lib/types"
import { cn } from "@/lib/utils"
import { BRAND_CLASSES, RoundtableSpeakerAvatar } from "./roundtable-transcript"
import { RoundtableTopicInput } from "./roundtable-topic-input"

/** Fixed candidates. Availability comes from `roundtable_agents`. */
export const ROUNDTABLE_AGENTS = [
  "grok",
  "antigravity",
  "cursor",
  "codex",
  "code_buddy",
] as const

export const ROUNDTABLE_MIN_MEMBERS = 2
export const ROUNDTABLE_MAX_MEMBERS = 7

const AGENT_LABELS: Record<string, string> = {
  grok: "Grok",
  antigravity: "Antigravity",
  cursor: "Cursor",
  codex: "Codex",
  code_buddy: "CodeBuddy",
}

export function roundtableAgentLabel(agent: string) {
  return AGENT_LABELS[agent] ?? agent
}

type Translate = ReturnType<typeof useTranslations<"Roundtable">>
type TranslateKey = Parameters<Translate>[0]

const READINESS_REASONS = new Set([
  "adapter_unqualified",
  "provider_unqualified",
  "provider_credential_missing",
  "qualification_policy_changed",
  "product_disabled",
  "unknown_agent",
  "effort_unqualified",
  "codebuddy_profile_missing",
  "codebuddy_profile_disabled",
  "codebuddy_profile_rejected",
  "profile",
])

/** Inline reason for an agent that cannot take a seat yet. */
export function agentStatusReason(row: RoundtableAgentStatus, t: Translate) {
  const key = agentStatusReasonKey(row)
  switch (key) {
    case "credential_missing":
      return row.credential_keys.length
        ? t("agentStatus_credential_missing", {
            keys: row.credential_keys.join(", "),
          })
        : t("agentStatus_credential_missing_login")
    case "unqualified":
      return row.last_qualification.failed_checks.length
        ? t("agentStatus_unqualified", {
            checks: row.last_qualification.failed_checks.slice(0, 3).join(", "),
          })
        : t("agentStatus_unqualified_plain")
    case "version_mismatch":
      return t("agentStatus_version_mismatch", {
        installed: row.installed_version ?? "?",
        profile: row.profile_version ?? "?",
      })
    case "disabled":
    case "not_installed":
    case "unsupported":
      return t(`agentStatus_${key}` as TranslateKey)
    default:
      return t("agentStatus_unqualified_plain")
  }
}

/** A readiness or run error that names its seat: "Seat 2 (Cursor): …".
 * Returns null when the error carries no seat. */
export function seatErrorMessage(error: unknown, t: Translate): string | null {
  const seat = namedSeat(error)
  if (!seat) return null
  const reason = errorReason(error)
  const text =
    reason && READINESS_REASONS.has(reason)
      ? t(`readiness_${reason}` as TranslateKey)
      : (reason ?? t("readiness_unknown"))
  return t("seatError", {
    seat: seat.ordinal + 1,
    agent: roundtableAgentLabel(seat.agent),
    reason: text,
  })
}

export interface ComposerMember {
  role: string
  agent: string
  /** Codeg model provider id; empty means the agent's qualified default. */
  providerId: string
  /** Saved CodeBuddy profile id; empty means the agent's defaults. */
  profileId: string
}

/** Native select styled like the shared Input. */
export const SELECT_CLASS =
  "h-9 w-full min-w-0 rounded-4xl border border-input bg-input/30 px-3 text-sm outline-none transition-colors focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 disabled:cursor-not-allowed disabled:opacity-50"

/** Numbered step with a heading, used by the create flow. */
export function RoundtableStep({
  step,
  title,
  description,
  aside,
  children,
}: {
  step: number
  title: string
  description?: ReactNode
  aside?: ReactNode
  children: ReactNode
}) {
  return (
    <section className="flex min-w-0 flex-col gap-4 border-t pt-5 first:border-t-0 first:pt-0">
      <div className="flex min-w-0 items-start gap-3">
        <span
          aria-hidden="true"
          className="grid size-7 shrink-0 place-items-center rounded-full bg-primary text-xs font-semibold text-primary-foreground tabular-nums"
        >
          {step}
        </span>
        <div className="min-w-0 flex-1 space-y-1 pt-0.5">
          <h3 className="text-base leading-6 font-semibold">{title}</h3>
          {description ? (
            <p className="text-sm leading-relaxed text-muted-foreground">
              {description}
            </p>
          ) : null}
        </div>
        {aside ? <div className="shrink-0 pt-0.5">{aside}</div> : null}
      </div>
      <div className="min-w-0 sm:ps-10">{children}</div>
    </section>
  )
}

export function RoundtableComposer({
  workspaceField,
  sourceRoot,
  workspaceId,
  topic,
  onTopicChange,
  showSources,
  sourcePaths,
  onSourcePathsChange,
  sourceCount,
  sourceValid,
  members,
  seatBrands,
  providers,
  onMemberChange,
  onAddMember,
  onRemoveMember,
  moderator,
  onModeratorChange,
  rounds,
  onRoundsChange,
  concurrency,
  onConcurrencyChange,
  budgetMinutes,
  agentStatus,
  agentStatusLoading = false,
  onRecheckAgents,
}: {
  /** `roundtable_agents` rows by agent; empty until loaded. */
  agentStatus?: Map<string, RoundtableAgentStatus>
  agentStatusLoading?: boolean
  /** Reload the agent status (settings, installs, qualification). */
  onRecheckAgents?: () => void
  topic: string
  onTopicChange: (value: string) => void
  showSources: boolean
  sourcePaths: string
  onSourcePathsChange: (value: string) => void
  sourceCount: number
  sourceValid: boolean
  members: ComposerMember[]
  seatBrands: Map<number, SeatBrand>
  providers: ModelProviderInfo[]
  onMemberChange: (index: number, patch: Partial<ComposerMember>) => void
  onAddMember: () => void
  onRemoveMember: (index: number) => void
  moderator: number
  onModeratorChange: (index: number) => void
  rounds: number
  onRoundsChange: (value: number) => void
  concurrency: number
  onConcurrencyChange: (value: number) => void
  budgetMinutes: number
  /** Workspace picker rendered as the first step of the create flow. */
  workspaceField?: ReactNode
  /** Absolute root that workspace-relative source paths resolve against. */
  sourceRoot?: string
  /** Registered folder id; scopes the `@` file picker. */
  workspaceId?: string
}) {
  const t = useTranslations("Roundtable")
  const uid = useId()
  const [topicTouched, setTopicTouched] = useState(false)
  const topicMissing = topicTouched && !topic.trim()
  const topicErrorId = `${uid}-topic-error`
  const sourceErrorId = `${uid}-source-error`
  const sourceHelpId = `${uid}-source-help`
  const canRemove = members.length > ROUNDTABLE_MIN_MEMBERS
  const offset = workspaceField ? 1 : 0
  const status = agentStatus ?? new Map<string, RoundtableAgentStatus>()
  const orderedAgents = sortRoundtableAgents(ROUNDTABLE_AGENTS, status)
  const notReady = orderedAgents
    .map((agent) => status.get(agent))
    .filter(
      (row): row is RoundtableAgentStatus => !!row && row.status !== "ready"
    )

  return (
    <>
      {workspaceField ? (
        <RoundtableStep
          step={1}
          title={t("workspace")}
          description={t("workspaceHelp")}
        >
          {workspaceField}
        </RoundtableStep>
      ) : null}
      <RoundtableStep
        step={1 + offset}
        title={t("topic")}
        description={t("topicHelp")}
      >
        <div className="flex flex-col gap-2">
          <RoundtableTopicInput
            value={topic}
            invalid={topicMissing}
            describedBy={topicMissing ? topicErrorId : undefined}
            workspaceId={workspaceId}
            workspacePath={sourceRoot}
            onBlur={() => setTopicTouched(true)}
            onChange={onTopicChange}
          />
          {sourceRoot ? (
            <p className="text-xs leading-relaxed text-muted-foreground">
              {t("topicMentionHelp")}
            </p>
          ) : null}
          {topicMissing ? (
            <p id={topicErrorId} className="text-sm text-destructive">
              {t("topicRequired")}
            </p>
          ) : null}
          {showSources ? (
            <details
              className="group rounded-xl border bg-muted/20 open:bg-muted/30"
              open={sourceCount > 0 || !sourceValid || undefined}
            >
              <summary className="flex cursor-pointer list-none items-center gap-2 rounded-xl px-3 py-2.5 text-sm font-medium select-none focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring [&::-webkit-details-marker]:hidden">
                <FileText
                  aria-hidden="true"
                  className="size-4 text-muted-foreground"
                />
                <span>{t("sourcesOptional")}</span>
                {sourceCount > 0 ? (
                  <Badge variant="secondary">
                    {t("sourcesCount", { count: sourceCount })}
                  </Badge>
                ) : null}
                <Plus
                  aria-hidden="true"
                  className="ms-auto size-4 text-muted-foreground transition-transform group-open:rotate-45"
                />
              </summary>
              <div className="flex flex-col gap-2 px-3 pb-3">
                <Textarea
                  aria-label={t("selectedSourcePaths")}
                  aria-invalid={!sourceValid || undefined}
                  aria-describedby={`${sourceHelpId}${!sourceValid ? ` ${sourceErrorId}` : ""}`}
                  className="bg-background font-mono text-xs md:text-xs"
                  value={sourcePaths}
                  placeholder={t("sourcePathsPlaceholder")}
                  onChange={(event) => onSourcePathsChange(event.target.value)}
                />
                {!sourceValid ? (
                  <p
                    id={sourceErrorId}
                    role="alert"
                    className="text-sm text-destructive"
                  >
                    {t("invalidSourceSelection")}
                  </p>
                ) : null}
                <div
                  id={sourceHelpId}
                  className="space-y-1 text-xs leading-relaxed text-muted-foreground"
                >
                  {sourceRoot ? (
                    <p className="[overflow-wrap:anywhere]">
                      {t("sourceRootHelp", { path: sourceRoot })}
                    </p>
                  ) : null}
                  <p>{t("sourceSelectionHelp")}</p>
                  <p>{t("sourceRetentionNotice")}</p>
                </div>
              </div>
            </details>
          ) : null}
        </div>
      </RoundtableStep>

      <RoundtableStep
        step={2 + offset}
        title={t("members")}
        description={t("membersHelp")}
        aside={
          <Badge variant="outline" className="tabular-nums">
            {members.length}/{ROUNDTABLE_MAX_MEMBERS}
          </Badge>
        }
      >
        {notReady.length > 0 ? (
          <div
            data-testid="roundtable-agent-availability"
            className="mb-3 flex min-w-0 flex-col gap-1.5 rounded-xl border border-dashed bg-muted/20 p-3"
          >
            <p className="text-xs font-medium text-muted-foreground">
              {t("agentAvailability")}
            </p>
            <ul className="flex min-w-0 flex-col gap-1.5">
              {notReady.map((row) => {
                const unqualified =
                  row.status === "unqualified" ||
                  row.status === "credential_missing"
                return (
                  <li
                    key={row.agent}
                    data-agent={row.agent}
                    data-status={row.status}
                    className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 text-xs"
                  >
                    <AgentIcon
                      agentType={row.agent}
                      className="size-3.5 shrink-0 opacity-60"
                    />
                    <span className="font-medium text-muted-foreground">
                      {roundtableAgentLabel(row.agent)}
                    </span>
                    <span className="min-w-0 flex-1 text-muted-foreground [overflow-wrap:anywhere]">
                      {agentStatusReason(row, t)}
                    </span>
                    {unqualified && onRecheckAgents ? (
                      <Button
                        type="button"
                        variant="outline"
                        size="xs"
                        disabled={agentStatusLoading}
                        aria-label={`${t("agentRecheck")} ${roundtableAgentLabel(row.agent)}`}
                        title={t("agentRecheckHelp")}
                        onClick={onRecheckAgents}
                      >
                        {agentStatusLoading ? (
                          <Loader2
                            aria-hidden="true"
                            className="animate-spin"
                          />
                        ) : (
                          <RefreshCw aria-hidden="true" />
                        )}
                        {agentStatusLoading
                          ? t("agentRechecking")
                          : t("agentRecheck")}
                      </Button>
                    ) : null}
                  </li>
                )
              })}
            </ul>
          </div>
        ) : null}
        <ol className="grid min-w-0 gap-3 lg:grid-cols-2">
          {members.map((member, index) => {
            const seat = seatBrands.get(index) ?? {
              key: roundtableBrandKey(member.agent),
              variant: 0,
              shared: false,
              brand: roundtableBrand(roundtableBrandKey(member.agent)),
            }
            const isModerator = moderator === index
            const knownProvider = providers.some(
              (provider) => String(provider.id) === member.providerId
            )
            const providerName = member.providerId
              ? (providers.find(
                  (provider) => String(provider.id) === member.providerId
                )?.name ?? member.providerId)
              : t("agentDefaultProvider")
            return (
              <li
                key={index}
                data-testid="roundtable-member-card"
                data-brand={seat.key}
                style={brandStyle(seat.brand)}
                className={cn(
                  "@container relative flex min-w-0 flex-col gap-3 rounded-xl border bg-background p-3 shadow-xs transition-colors sm:p-4",
                  isModerator && BRAND_CLASSES.border
                )}
              >
                <div className="flex min-w-0 items-center gap-3">
                  <RoundtableSpeakerAvatar
                    agent={member.agent}
                    seat={seat}
                    seatNumber={index + 1}
                  />
                  <div className="min-w-0 flex-1">
                    <p className="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm leading-5 font-medium">
                      <span>
                        {t("member")} {index + 1}
                      </span>
                      {isModerator ? (
                        <Badge variant="secondary" className="gap-1">
                          <Gavel aria-hidden="true" />
                          {t("moderator")}
                        </Badge>
                      ) : null}
                    </p>
                    <p className="truncate text-xs leading-5 text-muted-foreground">
                      {roundtableAgentLabel(member.agent)} · {providerName}
                    </p>
                  </div>
                  {canRemove ? (
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon-sm"
                      className="-me-1 -mt-1 shrink-0 self-start text-muted-foreground hover:text-foreground"
                      aria-label={`${t("removeMember")} ${index + 1}`}
                      title={t("removeMember")}
                      onClick={() => onRemoveMember(index)}
                    >
                      <X aria-hidden="true" />
                    </Button>
                  ) : null}
                </div>
                <label className="grid gap-1.5 text-xs font-medium text-muted-foreground">
                  {t("role")}
                  <Input
                    aria-label={`${t("role")} ${index + 1}`}
                    placeholder={t("rolePlaceholder")}
                    className="bg-background text-foreground"
                    value={member.role}
                    onChange={(event) =>
                      onMemberChange(index, { role: event.target.value })
                    }
                  />
                </label>
                <fieldset className="min-w-0">
                  <legend className="mb-1.5 text-xs font-medium text-muted-foreground">
                    <span aria-hidden="true">{t("agent")}</span>
                    <span className="sr-only">
                      {t("agent")} {index + 1}
                    </span>
                  </legend>
                  <div className="grid grid-cols-2 gap-1.5 @md:grid-cols-3 @xl:grid-cols-5">
                    {orderedAgents.map((agent) => {
                      const key = roundtableBrandKey(agent)
                      const checked = member.agent === agent
                      const row = status.get(agent)
                      const selectable = isAgentSelectable(agent, status)
                      return (
                        <label
                          key={agent}
                          data-agent={agent}
                          data-available={selectable ? "true" : "false"}
                          title={
                            row && !selectable
                              ? agentStatusReason(row, t)
                              : undefined
                          }
                          style={brandStyle(roundtableBrand(key))}
                          className={cn(
                            "relative flex min-h-9 min-w-0 items-center gap-1.5 rounded-full border px-2.5 py-1.5 text-xs font-medium transition-colors select-none has-[:focus-visible]:ring-[3px] has-[:focus-visible]:ring-ring/50",
                            selectable
                              ? "cursor-pointer hover:bg-muted"
                              : "cursor-not-allowed border-dashed opacity-50",
                            checked
                              ? cn(
                                  "border-(--rt-c) bg-(--rt-c)/8 dark:border-(--rt-c-dark) dark:bg-(--rt-c-dark)/15",
                                  BRAND_CLASSES.text
                                )
                              : "text-muted-foreground"
                          )}
                        >
                          <input
                            type="radio"
                            className="sr-only"
                            name={`${uid}-agent-${index}`}
                            value={agent}
                            checked={checked}
                            disabled={!selectable && !checked}
                            onChange={() => onMemberChange(index, { agent })}
                          />
                          <AgentIcon
                            agentType={agent}
                            className={cn(
                              "size-3.5 shrink-0",
                              checked && BRAND_CLASSES.text
                            )}
                          />
                          <span className="truncate">
                            {roundtableAgentLabel(agent)}
                          </span>
                        </label>
                      )
                    })}
                  </div>
                  {(() => {
                    const row = status.get(member.agent)
                    if (!row || row.status === "ready") return null
                    return (
                      <p
                        role="note"
                        className="mt-1.5 flex items-start gap-1.5 text-xs text-amber-700 dark:text-amber-300"
                      >
                        <CircleAlert
                          aria-hidden="true"
                          className="mt-0.5 size-3.5 shrink-0"
                        />
                        <span className="[overflow-wrap:anywhere]">
                          {t("agentNotReadySeat", {
                            agent: roundtableAgentLabel(member.agent),
                            reason: agentStatusReason(row, t),
                          })}
                        </span>
                      </p>
                    )
                  })()}
                </fieldset>
                <div className="flex min-w-0 flex-wrap items-end gap-3">
                  <label className="grid min-w-40 flex-1 gap-1.5 text-xs font-medium text-muted-foreground">
                    {t("provider")}
                    <select
                      aria-label={`${t("provider")} ${index + 1}`}
                      className={cn(
                        SELECT_CLASS,
                        "bg-background text-foreground"
                      )}
                      value={member.providerId}
                      onChange={(event) =>
                        onMemberChange(index, {
                          providerId: event.target.value,
                        })
                      }
                    >
                      <option value="">{t("agentDefaultProvider")}</option>
                      {providers.map((provider) => (
                        <option key={provider.id} value={String(provider.id)}>
                          {provider.name}
                        </option>
                      ))}
                      {member.providerId && !knownProvider ? (
                        <option value={member.providerId}>
                          {member.providerId}
                        </option>
                      ) : null}
                    </select>
                  </label>
                  {member.agent === "code_buddy"
                    ? (() => {
                        const profiles = seatProfiles(member.agent, status)
                        const known = profiles.some(
                          (profile) => profile.id === member.profileId
                        )
                        return (
                          <label className="grid min-w-40 flex-1 gap-1.5 text-xs font-medium text-muted-foreground">
                            {t("codeBuddyProfile")}
                            <select
                              aria-label={`${t("codeBuddyProfile")} ${index + 1}`}
                              className={cn(
                                SELECT_CLASS,
                                "bg-background text-foreground"
                              )}
                              value={member.profileId}
                              disabled={
                                profiles.length === 0 && !member.profileId
                              }
                              onChange={(event) =>
                                onMemberChange(index, {
                                  profileId: event.target.value,
                                })
                              }
                            >
                              <option value="">
                                {profiles.length === 0
                                  ? t("codeBuddyProfileNone")
                                  : t("codeBuddyProfileDefault")}
                              </option>
                              {profiles.map((profile) => (
                                <option key={profile.id} value={profile.id}>
                                  {profile.model
                                    ? `${profile.name} · ${profile.model}`
                                    : profile.name}
                                </option>
                              ))}
                              {member.profileId && !known ? (
                                <option value={member.profileId}>
                                  {t("codeBuddyProfileMissing")}
                                </option>
                              ) : null}
                            </select>
                          </label>
                        )
                      })()
                    : null}
                  <label
                    className={cn(
                      "flex h-9 cursor-pointer items-center gap-2 rounded-full border px-3 text-xs font-medium transition-colors select-none hover:bg-muted has-[:focus-visible]:ring-[3px] has-[:focus-visible]:ring-ring/50",
                      isModerator
                        ? cn(
                            "border-(--rt-c) dark:border-(--rt-c-dark)",
                            BRAND_CLASSES.text
                          )
                        : "text-muted-foreground"
                    )}
                  >
                    <input
                      type="radio"
                      className="size-3.5 accent-primary"
                      name={`${uid}-moderator`}
                      aria-label={`${t("moderator")} ${index + 1}`}
                      checked={isModerator}
                      onChange={() => onModeratorChange(index)}
                    />
                    <Gavel aria-hidden="true" className="size-3.5" />
                    {t("moderator")}
                  </label>
                </div>
              </li>
            )
          })}
          {members.length < ROUNDTABLE_MAX_MEMBERS ? (
            <li className="min-w-0">
              <button
                type="button"
                onClick={onAddMember}
                className="flex size-full min-h-24 w-full flex-col items-center justify-center gap-1.5 rounded-xl border border-dashed p-4 text-sm font-medium text-muted-foreground transition-colors hover:border-foreground/30 hover:bg-muted/40 hover:text-foreground focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring"
              >
                <Plus aria-hidden="true" className="size-5" />
                {t("addMember")}
              </button>
            </li>
          ) : null}
        </ol>
      </RoundtableStep>

      <RoundtableStep step={3 + offset} title={t("settings")}>
        <div className="grid min-w-0 gap-3 sm:grid-cols-3">
          <label className="flex min-w-0 flex-col gap-1.5 rounded-xl border bg-background p-3">
            <span className="text-sm font-medium">{t("rounds")}</span>
            <Input
              type="number"
              inputMode="numeric"
              min={0}
              max={5}
              className="w-24 bg-background tabular-nums"
              value={rounds}
              onChange={(event) =>
                onRoundsChange(
                  Math.max(0, Math.min(5, Number(event.target.value)))
                )
              }
            />
            <span className="text-xs leading-relaxed text-muted-foreground">
              {t("roundsHelp")}
            </span>
          </label>
          <label className="flex min-w-0 flex-col gap-1.5 rounded-xl border bg-background p-3">
            <span className="text-sm font-medium">{t("concurrency")}</span>
            <Input
              type="number"
              inputMode="numeric"
              min={1}
              max={members.length}
              className="w-24 bg-background tabular-nums"
              value={concurrency}
              onChange={(event) =>
                onConcurrencyChange(
                  Math.max(
                    1,
                    Math.min(members.length, Number(event.target.value))
                  )
                )
              }
            />
            <span className="text-xs leading-relaxed text-muted-foreground">
              {t("concurrencyHelp")}
            </span>
          </label>
          <div className="flex min-w-0 flex-col gap-1.5 rounded-xl border border-dashed bg-muted/20 p-3">
            <span className="text-sm font-medium">{t("budget")}</span>
            <span className="flex h-9 items-center gap-2 text-lg font-semibold tabular-nums">
              <Clock
                aria-hidden="true"
                className="size-4 text-muted-foreground"
              />
              {t("budgetMinutes", { minutes: budgetMinutes })}
            </span>
            <span className="text-xs leading-relaxed text-muted-foreground">
              {t("budgetHelp")}
            </span>
          </div>
        </div>
      </RoundtableStep>
    </>
  )
}
