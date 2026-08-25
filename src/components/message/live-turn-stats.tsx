"use client"

import {
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
} from "react"
import { useLocale, useTranslations } from "next-intl"
import type {
  LiveContentBlock,
  LiveMessage,
} from "@/contexts/acp-connections-context"
import { inferLiveToolName } from "@/lib/tool-call-normalization"
import { tryParseJsonForOwner } from "@/lib/try-parse-json"
import {
  formatElapsedLabel,
  type ElapsedUnitTranslator,
} from "@/lib/format-elapsed"
import {
  countUnifiedDiffLineChanges,
  estimateChangedLineStats,
} from "@/lib/line-change-stats"
import { FilePenLine, Plane, Timer } from "lucide-react"
import type { AgentType } from "@/lib/types"
import { AgentIcon } from "@/components/agent-icon"
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip"
import {
  EMPTY_REQUEST_USAGE,
  supportsRequestUsageDisplay,
} from "@/lib/request-usage-speed"
import type { RequestUsageSnapshot } from "@/lib/request-usage-speed"
import {
  getPublishedRequestUsage,
  subscribeRequestUsage,
} from "@/lib/request-usage-live"
import { cn } from "@/lib/utils"

export type LiveTurnStatusMode = "auto" | "waiting_for_subagents"

/** Live tok/s and generation-share are unfinished. Set true to show them. */
export const LIVE_TURN_REQUEST_USAGE_VISIBLE = false

interface LiveTurnStatsProps {
  message: LiveMessage
  agentType: AgentType
  conversationId?: number
  isStreaming?: boolean
  /**
   * `waiting_for_subagents` replaces the thinking/streaming label while keeping
   * the same elapsed / edit / tool-call layout for the bottom banner.
   */
  statusMode?: LiveTurnStatusMode
}

interface LineChangeStats {
  additions: number
  deletions: number
}

interface LiveEditStats extends LineChangeStats {
  files: number
}

function formatCompactInt(n: number, formatter: Intl.NumberFormat): string {
  if (n < 1000) return String(n)
  return formatter.format(n)
}

function asObject(value: unknown): Record<string, unknown> | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null
  return value as Record<string, unknown>
}

function parseInputObject(
  input: string | null,
  owner: object
): Record<string, unknown> | null {
  if (!input) return null
  return tryParseJsonForOwner(owner, input)
}

function unescapeInlineEscapes(text: string): string {
  return text
    .replace(/\\r\\n/g, "\n")
    .replace(/\\n/g, "\n")
    .replace(/\\t/g, "\t")
}

function looksLikeDiffPayload(input: string): boolean {
  const normalized = unescapeInlineEscapes(input)
  return (
    normalized.includes("*** Begin Patch") ||
    normalized.includes("*** Update File:") ||
    /^diff --git /m.test(normalized) ||
    (/^--- .+/m.test(normalized) && /^\+\+\+ .+/m.test(normalized)) ||
    /^@@ /m.test(normalized)
  )
}

function extractPatchText(
  rawInput: string | null,
  parsed: Record<string, unknown> | null
): string | null {
  if (!rawInput) return null
  if (looksLikeDiffPayload(rawInput)) return unescapeInlineEscapes(rawInput)
  if (!parsed) return null

  const candidates = [
    parsed.patch,
    parsed.diff,
    parsed.unified_diff,
    parsed.unifiedDiff,
    parsed.command,
    parsed.input,
    parsed.arguments,
    parsed.payload,
  ]

  for (const candidate of candidates) {
    if (typeof candidate !== "string") continue
    if (looksLikeDiffPayload(candidate)) return unescapeInlineEscapes(candidate)
  }

  return null
}

function addPathIfValid(paths: Set<string>, value: unknown): void {
  if (typeof value !== "string") return
  const path = value.trim()
  if (!path) return
  paths.add(path)
}

function collectParsedPaths(
  parsed: Record<string, unknown> | null
): Set<string> {
  const paths = new Set<string>()
  if (!parsed) return paths

  addPathIfValid(
    paths,
    parsed.file_path ?? parsed.filePath ?? parsed.path ?? parsed.notebook_path
  )

  const changes = asObject(parsed.changes)
  if (changes) {
    for (const path of Object.keys(changes)) {
      addPathIfValid(paths, path)
    }
  }

  return paths
}

function parseApplyPatchStats(patch: string): {
  files: Set<string>
  additions: number
  deletions: number
} {
  const files = new Set<string>()
  let additions = 0
  let deletions = 0

  for (const line of patch.split("\n")) {
    if (line.startsWith("*** Add File: ")) {
      addPathIfValid(files, line.slice(14))
      continue
    }
    if (line.startsWith("*** Update File: ")) {
      addPathIfValid(files, line.slice(17))
      continue
    }
    if (line.startsWith("*** Delete File: ")) {
      addPathIfValid(files, line.slice(17))
      continue
    }
    if (line.startsWith("+++ ")) {
      const normalized = line.slice(4).replace(/^b\//, "").trim()
      if (normalized && normalized !== "/dev/null") {
        files.add(normalized)
      }
      continue
    }
    if (line.startsWith("+") && !line.startsWith("+++")) additions += 1
    if (line.startsWith("-") && !line.startsWith("---")) deletions += 1
  }

  return { files, additions, deletions }
}

function extractEditStats(parsed: Record<string, unknown>): LineChangeStats {
  const changes = asObject(parsed.changes)
  if (changes) {
    let additions = 0
    let deletions = 0

    for (const change of Object.values(changes)) {
      const record = asObject(change)
      if (!record) continue

      const unifiedDiff =
        (typeof record.unifiedDiff === "string" && record.unifiedDiff) ||
        (typeof record.unified_diff === "string" && record.unified_diff) ||
        null

      if (unifiedDiff) {
        const stats = countUnifiedDiffLineChanges(unifiedDiff)
        additions += stats.additions
        deletions += stats.deletions
        continue
      }

      const oldString =
        (typeof record.oldText === "string" && record.oldText) ||
        (typeof record.old_string === "string" && record.old_string) ||
        ""
      const newString =
        (typeof record.newText === "string" && record.newText) ||
        (typeof record.new_string === "string" && record.new_string) ||
        ""

      const estimated = estimateChangedLineStats(oldString, newString)
      additions += estimated.additions
      deletions += estimated.deletions
    }

    return { additions, deletions }
  }

  const oldString =
    (typeof parsed.old_string === "string" && parsed.old_string) ||
    (typeof parsed.oldText === "string" && parsed.oldText) ||
    ""
  const newString =
    (typeof parsed.new_string === "string" && parsed.new_string) ||
    (typeof parsed.newText === "string" && parsed.newText) ||
    ""

  return estimateChangedLineStats(oldString, newString)
}

function extractWriteStats(parsed: Record<string, unknown>): LineChangeStats {
  const content =
    (typeof parsed.content === "string" && parsed.content) ||
    (typeof parsed.new_source === "string" && parsed.new_source) ||
    ""

  const additions = content.length === 0 ? 0 : content.split("\n").length
  return { additions, deletions: 0 }
}

interface BlockEditContribution {
  files: string[]
  additions: number
  deletions: number
}

// Parsing a tool call's `raw_input` (JSON.parse + diff line counting) is the
// expensive part of the live edit stats, and it re-runs on every streaming
// token because the `message` reference changes each token. A completed
// tool_call block keeps a stable reference across tokens (the reducer rebuilds
// only the block that changed — see acp-connections-context), so cache each
// block's contribution keyed on the block object. Only the block currently
// being updated re-parses; the rest are O(1) lookups. Keying on the block ref
// is sound because a block's ref changes iff its content changes, and the
// WeakMap lets dropped blocks be collected.
const blockEditContributionCache = new WeakMap<
  LiveContentBlock,
  BlockEditContribution | null
>()

function computeBlockEditContribution(
  block: LiveContentBlock
): BlockEditContribution | null {
  if (block.type !== "tool_call") return null
  const owner = block.info
  const toolName = inferLiveToolName({
    title: block.info.title,
    kind: block.info.kind,
    rawInput: block.info.raw_input,
    meta: block.info.meta,
    owner,
  })
  if (toolName !== "edit" && toolName !== "write" && toolName !== "apply_patch")
    return null

  const files = new Set<string>()
  let additions = 0
  let deletions = 0

  const parsed = parseInputObject(block.info.raw_input, owner)
  for (const path of collectParsedPaths(parsed)) files.add(path)

  if (toolName === "apply_patch") {
    const patch = extractPatchText(block.info.raw_input, parsed)
    if (patch) {
      const stats = parseApplyPatchStats(patch)
      for (const path of stats.files) files.add(path)
      additions += stats.additions
      deletions += stats.deletions
    }
  } else if (parsed) {
    const stats =
      toolName === "edit" ? extractEditStats(parsed) : extractWriteStats(parsed)
    additions += stats.additions
    deletions += stats.deletions
  }

  return { files: [...files], additions, deletions }
}

function blockEditContribution(
  block: LiveContentBlock
): BlockEditContribution | null {
  const cached = blockEditContributionCache.get(block)
  if (cached !== undefined) return cached
  const contribution = computeBlockEditContribution(block)
  blockEditContributionCache.set(block, contribution)
  return contribution
}

export function extractLiveEditStats(message: LiveMessage): LiveEditStats {
  const files = new Set<string>()
  let additions = 0
  let deletions = 0

  for (const block of message.content) {
    const contribution = blockEditContribution(block)
    if (!contribution) continue
    for (const path of contribution.files) files.add(path)
    additions += contribution.additions
    deletions += contribution.deletions
  }

  return { files: files.size, additions, deletions }
}

interface DisplayedUsage {
  tps: number
  generationMs: number
}

interface UsageTransition {
  startedAt: number
  start: DisplayedUsage
  target: DisplayedUsage
}

const ZERO_DISPLAYED_USAGE: DisplayedUsage = { tps: 0, generationMs: 0 }
const USAGE_TRANSITION_MS = 5_000
const USAGE_TICK_MS = 33

function easeOutCubic(progress: number): number {
  return 1 - Math.pow(1 - progress, 3)
}

function interpolateUsage(
  transition: UsageTransition,
  now: number
): DisplayedUsage {
  const progress = Math.min(
    1,
    Math.max(0, (now - transition.startedAt) / USAGE_TRANSITION_MS)
  )
  const eased = easeOutCubic(progress)
  return {
    tps:
      transition.start.tps +
      (transition.target.tps - transition.start.tps) * eased,
    generationMs:
      transition.start.generationMs +
      (transition.target.generationMs - transition.start.generationMs) * eased,
  }
}

function useAnimatedRequestUsage(
  conversationId: number | null | undefined,
  enabled: boolean,
  snapshot: RequestUsageSnapshot
): DisplayedUsage {
  const [displayed, setDisplayed] = useState(ZERO_DISPLAYED_USAGE)
  const displayedRef = useRef(displayed)
  const transitionRef = useRef<UsageTransition | null>(null)
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null)
  const conversationRef = useRef(conversationId)

  useLayoutEffect(() => {
    const clearTargetInterval = () => {
      if (intervalRef.current !== null) {
        clearInterval(intervalRef.current)
        intervalRef.current = null
      }
    }
    const commit = (value: DisplayedUsage) => {
      displayedRef.current = value
      setDisplayed(value)
    }
    const changedConversation = conversationRef.current !== conversationId
    conversationRef.current = conversationId
    const validTarget =
      enabled &&
      snapshot.sampleCount > 0 &&
      Number.isFinite(snapshot.tps) &&
      snapshot.tps > 0 &&
      Number.isFinite(snapshot.generationMs) &&
      snapshot.generationMs > 0

    if (!validTarget) {
      clearTargetInterval()
      transitionRef.current = null
      commit(ZERO_DISPLAYED_USAGE)
      return clearTargetInterval
    }

    const now = performance.now()
    const start = changedConversation
      ? ZERO_DISPLAYED_USAGE
      : transitionRef.current
        ? interpolateUsage(transitionRef.current, now)
        : displayedRef.current
    clearTargetInterval()
    commit(start)
    transitionRef.current = {
      startedAt: now,
      start,
      target: {
        tps: snapshot.tps,
        generationMs: snapshot.generationMs,
      },
    }
    intervalRef.current = setInterval(() => {
      const transition = transitionRef.current
      if (!transition) return
      const tickNow = performance.now()
      if (tickNow - transition.startedAt >= USAGE_TRANSITION_MS) {
        clearTargetInterval()
        transitionRef.current = null
        commit(transition.target)
        return
      }
      commit(interpolateUsage(transition, tickNow))
    }, USAGE_TICK_MS)

    return clearTargetInterval
  }, [
    conversationId,
    enabled,
    snapshot.generationMs,
    snapshot.sampleCount,
    snapshot.tps,
  ])

  return displayed
}

function isVisibleTps(tps: number): boolean {
  return Number.isFinite(tps) && tps > 0 && tps.toFixed(1) !== "0.0"
}

function generationSharePercent(
  generationMs: number,
  elapsedMs: number
): number {
  if (
    !Number.isFinite(generationMs) ||
    generationMs <= 0 ||
    !Number.isFinite(elapsedMs) ||
    elapsedMs <= 0
  ) {
    return 0
  }
  return Math.min(100, Math.round((generationMs / elapsedMs) * 100))
}

function isVisibleGeneration(
  generationMs: number,
  elapsedMs: number,
  t: ElapsedUnitTranslator
): boolean {
  if (!Number.isFinite(generationMs) || generationMs <= 0) return false
  if (formatElapsedLabel(generationMs, t) === formatElapsedLabel(0, t)) {
    return false
  }
  return generationSharePercent(generationMs, elapsedMs) > 0
}

function ApproximationMarker({
  label,
  tooltip,
}: {
  label: string
  tooltip: string
}) {
  const [open, setOpen] = useState(false)
  return (
    <Tooltip open={open} onOpenChange={setOpen}>
      <TooltipTrigger asChild>
        <button
          type="button"
          aria-label={label}
          className="shrink-0 cursor-help text-foreground/80 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
          onClick={() => setOpen((value) => !value)}
        >
          ≈
        </button>
      </TooltipTrigger>
      <TooltipContent side="top">{tooltip}</TooltipContent>
    </Tooltip>
  )
}

export function LiveTurnStats({
  message,
  agentType,
  conversationId,
  isStreaming = true,
  statusMode = "auto",
}: LiveTurnStatsProps) {
  const locale = useLocale()
  const t = useTranslations("Folder.chat.liveTurnStats")
  const [elapsed, setElapsed] = useState(() => Date.now() - message.startedAt)
  const editStats = useMemo(() => extractLiveEditStats(message), [message])
  const showUsage =
    LIVE_TURN_REQUEST_USAGE_VISIBLE && supportsRequestUsageDisplay(agentType)
  const usageSnap = useSyncExternalStore(
    subscribeRequestUsage,
    () =>
      conversationId != null
        ? getPublishedRequestUsage(conversationId)
        : EMPTY_REQUEST_USAGE,
    () => EMPTY_REQUEST_USAGE
  )
  const displayedUsage = useAnimatedRequestUsage(
    conversationId,
    showUsage,
    usageSnap
  )
  const tpsLabel = displayedUsage.tps.toFixed(1)
  const validTps = isVisibleTps(displayedUsage.tps)
  // Keep the slots in the row only when a real target exists (covers the
  // 5s tween from 0). Empty/invalid snapshots must collapse — previously
  // `invisible` plus leftover rem boxes sat to the right of a justify-center
  // row, so "streaming | 26s" looked left of center and "tok/s ≈" had a
  // trailing gap.
  const reserveTps =
    showUsage && usageSnap.sampleCount > 0 && isVisibleTps(usageSnap.tps)
  const generationShare = generationSharePercent(
    displayedUsage.generationMs,
    elapsed
  )
  const generationLabel = isVisibleGeneration(
    displayedUsage.generationMs,
    elapsed,
    t
  )
    ? formatElapsedLabel(displayedUsage.generationMs, t)
    : ""
  const validGeneration = generationLabel !== ""
  const reserveGeneration =
    showUsage &&
    usageSnap.sampleCount > 0 &&
    isVisibleGeneration(usageSnap.generationMs, elapsed, t)
  const compactNumberFormatter = useMemo(
    () =>
      new Intl.NumberFormat(locale, {
        notation: "compact",
        maximumFractionDigits: 1,
      }),
    [locale]
  )

  useEffect(() => {
    const timer = setInterval(() => {
      setElapsed(Date.now() - message.startedAt)
    }, 1_000)
    return () => clearInterval(timer)
  }, [message.startedAt])

  const hasThinkingBlock = message.content.some((b) => b.type === "thinking")

  // Only active streams should show thinking/streaming state.
  const lastBlock = message.content[message.content.length - 1]
  const isThinking =
    statusMode === "auto" &&
    isStreaming &&
    hasThinkingBlock &&
    message.content.length <= 1 &&
    lastBlock?.type === "thinking"

  const statusLabel =
    statusMode === "waiting_for_subagents"
      ? t("waitingForSubagents")
      : isThinking
        ? t("thinking")
        : t("streaming")

  const elapsedLabel = formatElapsedLabel(elapsed, t)

  return (
    <div
      className="@container/turnstats shrink-0"
      data-testid="live-turn-stats"
      data-status-mode={statusMode}
    >
      <div className="flex min-h-8 flex-wrap items-center justify-center gap-x-3 gap-y-1 px-4 py-1 text-xs leading-none text-muted-foreground">
        <AgentIcon
          agentType={agentType}
          className="h-3.5 w-3.5 animate-pulse"
        />
        <span data-testid="live-turn-stats-status">{statusLabel}</span>
        <span className="text-border leading-none">|</span>
        <span className="inline-flex items-center gap-1 leading-none">
          <Timer className="h-3 w-3 shrink-0" />
          {elapsedLabel}
        </span>
        {editStats.files > 0 && (
          <>
            <span className="hidden text-border leading-none @[24rem]/turnstats:inline">
              |
            </span>
            <span className="hidden items-center gap-1 leading-none @[24rem]/turnstats:inline-flex">
              <FilePenLine className="h-3 w-3 shrink-0" />
              {editStats.files}F +
              {formatCompactInt(editStats.additions, compactNumberFormatter)}/-
              {formatCompactInt(editStats.deletions, compactNumberFormatter)}
            </span>
          </>
        )}
        {showUsage && (
          <TooltipProvider delayDuration={0}>
            <span
              className={cn(
                "hidden text-border leading-none",
                reserveTps && "@[30rem]/turnstats:inline",
                reserveTps && !validTps && "invisible"
              )}
            >
              |
            </span>
            <span
              data-testid="output-speed-slot"
              className={cn(
                "hidden items-center gap-1 leading-none tabular-nums",
                reserveTps && "@[30rem]/turnstats:inline-flex",
                reserveTps && !validTps && "invisible"
              )}
              title={validTps ? t("outputSpeedTooltip") : undefined}
            >
              {validTps && (
                <>
                  <Plane
                    aria-label={t("outputSpeedAria")}
                    className="h-3 w-3 shrink-0"
                  />
                  <span>{tpsLabel} tok/s</span>
                  {usageSnap.estimatedSampleCount > 0 && (
                    <ApproximationMarker
                      label={t("estimatedAria")}
                      tooltip={t("estimatedTooltip")}
                    />
                  )}
                </>
              )}
            </span>
            <span
              className={cn(
                "hidden text-border leading-none",
                reserveGeneration && "@[36rem]/turnstats:inline",
                reserveGeneration && !validGeneration && "invisible"
              )}
            >
              |
            </span>
            <span
              data-testid="generation-share-slot"
              className={cn(
                "hidden items-center gap-1 leading-none tabular-nums",
                reserveGeneration && "@[36rem]/turnstats:inline-flex",
                reserveGeneration && !validGeneration && "invisible"
              )}
              title={
                validGeneration
                  ? t("generationShareTooltip", {
                      generation: generationLabel,
                      wall: formatElapsedLabel(elapsed, t),
                      percent: generationShare,
                    })
                  : undefined
              }
            >
              {validGeneration && (
                <>
                  {generationLabel}
                  <span className="text-muted-foreground/80">
                    ({generationShare}%)
                  </span>
                  {usageSnap.estimatedSampleCount > 0 && (
                    <ApproximationMarker
                      label={t("estimatedAria")}
                      tooltip={t("estimatedTooltip")}
                    />
                  )}
                </>
              )}
            </span>
          </TooltipProvider>
        )}
      </div>
    </div>
  )
}
