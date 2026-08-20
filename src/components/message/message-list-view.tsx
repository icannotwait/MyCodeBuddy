"use client"

import {
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react"
import {
  selectDelegationActivities,
  selectHistoricalTimelineTurns,
  selectTimelineTurns,
  useConversationRuntimeActions,
  useConversationRuntimeStore,
} from "@/stores/conversation-runtime-store"
import { useStreamingPerformanceFlag } from "@/lib/acp/streaming-performance-config"
import { streamingPerfRecorder } from "@/lib/perf/streaming-perf-recorder"
import {
  useHasLiveTranscript,
  useLiveTranscriptConversation,
  type LiveTranscriptSnapshot,
} from "@/stores/live-transcript-store"
import type {
  LiveContentBlock,
  LiveMessage,
} from "@/contexts/acp-connections-context"
import { useAgentThinkingVisibility } from "@/hooks/use-acp-agents"
import { isWindowedDetail } from "@/lib/turn-window"
import { ContentPartsRenderer } from "./content-parts-renderer"
import { LiveTranscriptRow } from "./live-transcript-row"
import { ContextCompactionCard } from "./context-compaction-card"
import { CollapsibleUserMessage } from "./collapsible-user-message"
import { isContextCompactionMeta } from "@/lib/context-compaction"
import {
  createMessageTurnAdapter,
  groupGoalRuns,
  mergeAdjacentToolGroups,
  mergeAdjacentDelegationStatusGroups,
  mergeAdjacentBackgroundTaskGroups,
  type AdaptedContentPart,
  type AdaptedMessage,
  type MessageTurnAdapter,
  type UserImageDisplay,
  type UserResourceDisplay,
} from "@/lib/adapters/ai-elements-adapter"
import { TurnStats } from "./turn-stats"
import { LiveTurnStats } from "./live-turn-stats"
import { ReplyArtifacts } from "./reply-artifacts"
import { UserResourceLinks } from "./user-resource-links"
import { UserImageAttachments } from "./user-image-attachments"
import { AgentPlanOverlay } from "@/components/chat/agent-plan-overlay"
import { SubAgentOverlay } from "@/components/chat/sub-agent-overlay"
import {
  inferLiveToolName,
  normalizeToolName,
} from "@/lib/tool-call-normalization"
import { isDelegateToAgentToolName } from "@/lib/delegation-card"
import type { DelegationCardSource } from "@/hooks/use-delegation-card-model"
import { useDurableDelegationSources } from "@/hooks/use-durable-delegation-sources"
import { mergeDelegationSourceLayers } from "@/lib/delegation-overlay-history"
import {
  dedupeDelegationActivities,
  deriveNativeActivitiesFromToolCalls,
} from "@/lib/delegation-activity"
import { projectDelegationTranscript } from "@/lib/delegation-transcript-projection"
import { filterConversationInterruptedParts } from "@/lib/delegation-conversation-interrupted"
import type { DelegationActivityView } from "@/lib/types"
import { projectNativeActivitiesFromTranscript } from "@/lib/acp/live-transcript-projector"
import {
  MessageThread,
  MessageThreadScrollButton,
} from "@/components/ai-elements/message-thread"
import {
  Message,
  MessageContent,
  MessageAction,
} from "@/components/ai-elements/message"
import {
  AlertCircle,
  CheckIcon,
  ChevronDown,
  ChevronRight,
  CircleStop,
  CopyIcon,
  History,
  Info,
  Loader2,
  Plus,
  RefreshCw,
  ListTodo,
} from "lucide-react"
import { useCreateTaskFromMessage } from "./use-create-task-from-message"
import { Button } from "@/components/ui/button"
import { useTranslations } from "next-intl"
import {
  buildPlanKey,
  extractLatestPlanEntriesFromMessages,
} from "@/lib/agent-plan"
import type {
  AgentType,
  AutonomousTurnOrigin,
  ConnectionStatus,
  MessageTurn,
  TurnOutcome,
} from "@/lib/types"
import { copyTextToClipboard } from "@/lib/utils"
import { VirtualizedMessageThread } from "@/components/message/virtualized-message-thread"
import {
  ConversationMessageNav,
  type MessageNavEntry,
} from "@/components/message/conversation-message-nav"
import type { MessageScrollContextValue } from "@/components/message/message-scroll-context"
import { InitialHistoryScrollController } from "./initial-history-scroll-controller"
import { extractSessionFilesGrouped } from "@/lib/session-files"
import { unescapeComposerText } from "@/lib/composer-copy-text"
import { useStickToBottomContext } from "use-stick-to-bottom"

interface MessageListViewProps {
  conversationId: number
  agentType: AgentType
  workspaceRootPath?: string | null
  connStatus?: ConnectionStatus | null
  isActive?: boolean
  sendSignal?: number
  detailLoading?: boolean
  detailError?: string | null
  /**
   * Set when the agent rejected `session/load` non-recoverably (e.g. the
   * historical session_id was deleted, or the conversation's folder is gone).
   * Replaces the message area only when nothing is renderable; when the local
   * DB has the message history, the transcript stays visible and the owning
   * panel surfaces this error as a banner in the composer area instead (with
   * Reload / New session actions), since the agent can't continue the thread.
   */
  acpLoadError?: string | null
  /** Stable backend code for the ACP load failure. */
  acpLoadErrorCode?: string | null
  hideEmptyState?: boolean
  onReload?: () => void
  onNewSession?: () => void
  /**
   * Renders the per-conversation message navigator rail. Enabled in the main
   * conversation view; disabled in compact embeds (e.g. the sub-agent dialog).
   */
  showMessageNav?: boolean
  /** Immutable mount-time eligibility supplied by the owning conversation view. */
  initialHistoryScrollEligible?: boolean
  /** True only after a persisted detail payload has loaded successfully. */
  historyLoadComplete?: boolean
  /** Optional durable child turn id to reveal after the transcript loads. */
  focusTurnAnchor?: string | null
  /**
   * Parent continuation owns admission while subagents run (suspend + join).
   * When set, the bottom LiveTurnStats banner swaps the streaming label for
   * "waiting for subagents" while keeping elapsed / files / tool metrics.
   * Value is `armed_at` epoch ms for timer fallback when no latched live
   * message remains.
   */
  waitingForSubagentsArmedAtMs?: number | null
  onResumeRoot?: () => void | Promise<void>
  onOpenRootConversation?: (conversationId: number) => void | Promise<void>
  /**
   * Optional phase label for a user turn (work-task transcripts label each
   * engine-dispatched round: work / retry / return / merge). Called at render
   * time per user-role turn; MUST be pure — the thread is virtualized, so
   * items render in arbitrary order and multiplicity. `null` = no divider.
   */
  userTurnHeader?: ((group: ResolvedMessageGroup) => string | null) | null
}

export function canReloadSessionLoadError(
  code: string | null | undefined
): boolean {
  return code !== "legacy_cli_session"
}

type AutolinkableTextPart = Extract<AdaptedContentPart, { type: "text" }>

const EMPTY_AUTOLINKABLE_TEXT_PARTS: ReadonlySet<AutolinkableTextPart> =
  new Set()

export interface ResolvedMessageGroup {
  id: string
  role: "user" | "assistant" | "system"
  parts: AdaptedContentPart[]
  resources: UserResourceDisplay[]
  images: UserImageDisplay[]
  /**
   * Top-level adapted text parts from source-role `assistant` messages only.
   * Object-identity membership gates local-path autolinking; tool text that
   * is display-normalized to assistant is intentionally excluded.
   */
  autolinkableTextParts: ReadonlySet<AutolinkableTextPart>
  usage?: import("@/lib/types").TurnUsage | null
  duration_ms?: number | null
  generation_ms?: number | null
  generation_tokens?: number | null
  model?: string | null
  models?: string[]
  reasoning_effort?: string | null
  reasoning_efforts?: string[]
  /**
   * Wall-clock completion time supplied by the Rust parser. For merged
   * sub-turns this is the latest non-null completion across the run — the
   * post-turn metadata patch may sit on any sub-turn, not just the last.
   */
  completed_at?: string | null
  /**
   * Terminal turn outcome for interruption footers. For merged assistant runs
   * this is the last non-null outcome across the run.
   */
  outcome?: TurnOutcome | null
  /**
   * Provider-stamped autonomous origin. Present only on background
   * continuations; origin-only updates must produce a new group so memoized
   * assistant rows re-render the marker.
   */
  autonomous_origin?: AutonomousTurnOrigin | null
}

function topLevelAssistantTextParts(
  message: AdaptedMessage
): ReadonlySet<AutolinkableTextPart> {
  if (message.role !== "assistant") return EMPTY_AUTOLINKABLE_TEXT_PARTS
  const parts = message.content.filter(
    (part): part is AutolinkableTextPart => part.type === "text"
  )
  return parts.length > 0 ? new Set(parts) : EMPTY_AUTOLINKABLE_TEXT_PARTS
}

export type ThreadRenderItem =
  | {
      key: string
      kind: "turn"
      group: ResolvedMessageGroup
      phase: "persisted" | "optimistic" | "streaming"
      showStats: boolean
      isRoleTransition: boolean
      previousUserIndex: number | null
      /** Raw assistant sub-turn(s) that compose this reply — fed to the
       *  per-reply artifacts card so it can list files changed this reply. */
      sourceTurns: MessageTurn[]
    }
  | {
      key: string
      kind: "typing"
    }
  | {
      // A context-compaction event hoisted OUT of an assistant turn into its own
      // standalone timeline element. In history the compaction lands as its own
      // (assistant-role) turn between the reply that preceded `/compact` and the
      // next message; rendering it as a "turn" would let
      // `mergeConsecutiveAssistantTurns` fold it into the preceding reply (so the
      // divider showed up wedged before that reply's file cards + footer). As a
      // dedicated kind it breaks the assistant-merge run and renders as a
      // chrome-less centered divider in the correct between-turns position.
      key: string
      kind: "compaction"
      meta: Record<string, unknown> | null
    }

// Module-scope so the reference is stable across renders — lets the memoized
// VirtualizedMessageThread bail out when `items` is unchanged.
const getThreadItemKey = (item: ThreadRenderItem) => item.key

// Stable empty reference so the SubAgentOverlay memo can bail out when there
// are no delegations in the conversation.
const EMPTY_DELEGATIONS: DelegationCardSource[] = []

// Stable empty reference so the navigator memo / equality checks don't churn
// when a conversation has no user messages.
const EMPTY_NAV_ENTRIES: MessageNavEntry[] = []

// A single turn's `sourceTurns` is just `[turn]`. Cache the wrapper per turn
// object so an unchanged historical turn keeps a stable `sourceTurns` reference
// across streaming-token re-renders — that's the last prop preventing
// `HistoricalMessageGroup`'s memo from bailing out (its `group` and the
// phase-derived flags are already reference-/value-stable). The streaming turn
// is rebuilt every token, so it gets a fresh wrapper and still re-renders.
const sourceTurnsSingletonCache = new WeakMap<MessageTurn, MessageTurn[]>()
export function singletonSourceTurns(turn: MessageTurn): MessageTurn[] {
  let cached = sourceTurnsSingletonCache.get(turn)
  if (!cached) {
    cached = [turn]
    sourceTurnsSingletonCache.set(turn, cached)
  }
  return cached
}

// Collect the `delegate_to_agent` tool calls within a turn's adapted parts,
// recursing through tool-groups and goal-runs (a delegate call is normally a
// standalone part — `isAgentLikeToolName` keeps it out of tool-groups — but we
// scan nested containers defensively so a delegation is never missed).
function collectDelegationSources(
  parts: readonly AdaptedContentPart[],
  out: DelegationCardSource[],
  parentConversationId: number
): void {
  for (const part of parts) {
    if (part.type === "tool-call") {
      if (
        part.toolCallId &&
        isDelegateToAgentToolName(normalizeToolName(part.toolName))
      ) {
        out.push({
          parentToolUseId: part.toolCallId,
          parentConversationId,
          input: part.input ?? null,
          output: part.output ?? null,
          errorText: part.errorText ?? null,
          state: part.state,
          meta: part.meta ?? null,
        })
      }
    } else if (part.type === "delegation-work-unit") {
      collectDelegationSources(part.sources, out, parentConversationId)
    } else if (part.type === "tool-group") {
      collectDelegationSources(part.items, out, parentConversationId)
    } else if (part.type === "goal-run") {
      collectDelegationSources(part.items, out, parentConversationId)
    }
  }
}

const CollapsibleSystemMessage = memo(function CollapsibleSystemMessage({
  group,
  showThinking,
}: {
  group: ResolvedMessageGroup
  showThinking: boolean
}) {
  const [expanded, setExpanded] = useState(false)
  const t = useTranslations("Folder.chat.messageList")

  return (
    <div className="border rounded-md text-sm border-yellow-500/30 bg-yellow-500/5">
      <button
        onClick={() => setExpanded(!expanded)}
        className="flex items-center gap-2 w-full px-3 py-2.5 text-left hover:bg-yellow-500/10 transition-colors"
      >
        {expanded ? (
          <ChevronDown className="h-3.5 w-3.5 shrink-0 text-yellow-600 dark:text-yellow-500" />
        ) : (
          <ChevronRight className="h-3.5 w-3.5 shrink-0 text-yellow-600 dark:text-yellow-500" />
        )}
        <Info className="h-3.5 w-3.5 shrink-0 text-yellow-600 dark:text-yellow-500" />
        <span className="font-medium text-yellow-700 dark:text-yellow-400">
          {t("systemMessage")}
        </span>
      </button>
      {expanded && (
        <div className="px-3 pb-3 border-t border-yellow-500/20">
          <div className="text-sm text-muted-foreground mt-2.5 max-h-96 overflow-auto">
            <ContentPartsRenderer
              parts={group.parts}
              role={group.role}
              showThinking={showThinking}
            />
          </div>
        </div>
      )}
    </div>
  )
})

export function extractTextFromParts(parts: AdaptedContentPart[]): string {
  return parts
    .flatMap((part): string[] => {
      if (part.type === "text") return [part.text]
      if (part.type === "reasoning") return [part.content]
      if (part.type === "goal-run") return [extractTextFromParts(part.items)]
      return []
    })
    .filter((text) => text.length > 0)
    .join("\n")
}

type AssistantTurnItem = Extract<ThreadRenderItem, { kind: "turn" }>

function normalizeAutonomousOrigin(
  origin: AutonomousTurnOrigin | null | undefined
): AutonomousTurnOrigin | null {
  return origin ?? null
}

/** Stable episode key used as a merge boundary for autonomous assistant runs. */
function autonomousEpisodeKey(turnId: string): string {
  const grok = turnId.match(/^grok-autonomous:(.+):assistant:\d+$/)
  if (grok) return `grok-autonomous:${grok[1]}`
  const claudeOverlay = turnId.match(/^bg-(\d+)-\d+$/)
  if (claudeOverlay) return `bg-${claudeOverlay[1]}`
  return turnId
}

function assistantAutonomousIdentity(item: AssistantTurnItem): {
  origin: AutonomousTurnOrigin | null
  episode: string | null
} {
  const origin = normalizeAutonomousOrigin(
    item.group.autonomous_origin ?? item.sourceTurns[0]?.autonomous_origin
  )
  return {
    origin,
    episode: origin ? autonomousEpisodeKey(item.group.id) : null,
  }
}

function shouldFlushAutonomousAssistantRun(
  prev: AssistantTurnItem,
  next: AssistantTurnItem
): boolean {
  const left = assistantAutonomousIdentity(prev)
  const right = assistantAutonomousIdentity(next)
  if (left.origin !== right.origin) return true
  return left.origin != null && left.episode !== right.episode
}

/**
 * Cache entry for one merged assistant run, keyed on the run's FIRST member
 * group. Valid only while every member's group reference and item key still
 * match: group identity flows through the per-turn adapter + group caches, so
 * member-group equality implies unchanged content AND sourceTurns, while the
 * keys embed phase/role/id so identity or phase drift invalidates too. A run
 * containing the streaming turn misses every batch by construction (the
 * streaming turn re-adapts per batch) — that residual rebuild is the point;
 * purely historical runs hit and keep their group/parts/sourceTurns
 * references stable so HistoricalMessageGroup's memo bails out.
 */
export interface MergedAssistantRunCacheEntry {
  memberGroups: ResolvedMessageGroup[]
  memberKeys: string[]
  item: AssistantTurnItem
}
export type MergedAssistantRunCache = WeakMap<
  ResolvedMessageGroup,
  MergedAssistantRunCacheEntry
>

function isEmptyTurnItem(item: ThreadRenderItem): boolean {
  if (item.kind !== "turn") return false
  const g = item.group
  if (g.parts.length > 0) return false
  if (g.resources.length > 0) return false
  if (g.images.length > 0) return false
  // Outcome-only turns are grouping participants (footer must not be absorbed).
  if (g.outcome) return false
  return true
}

/**
 * When a resolved group's ONLY meaningful content is a single context-compaction
 * tool-call part, return that part's `_meta` (so the caller can hoist it to a
 * standalone `"compaction"` divider item); otherwise `null`. Empty text parts are
 * ignored so a bare compaction turn still qualifies. Scoped to assistant groups
 * with no user resources/images. A compaction part always carries a truthy
 * `_meta` (`contextCompaction` as the boolean marker or the 1.3.0+ versioned
 * object), so a non-null return is unambiguous.
 */
function compactionOnlyMeta(
  group: ResolvedMessageGroup
): Record<string, unknown> | null {
  if (group.role !== "assistant") return null
  if (group.resources.length > 0 || group.images.length > 0) return null
  const meaningful = group.parts.filter(
    (p) => !(p.type === "text" && p.text.trim().length === 0)
  )
  if (meaningful.length !== 1) return null
  const only = meaningful[0]
  if (only.type !== "tool-call" || !isContextCompactionMeta(only.meta)) {
    return null
  }
  return only.meta ?? null
}

/**
 * Collapse runs of consecutive assistant turn render items into a single
 * synthetic turn so tool-groups straddling a turn boundary fold into one
 * collapsible. Empty (no-content) turn items are treated as transparent and
 * do not break the run — that handles cases where parsers leave empty
 * placeholder turns between tool exchanges.
 *
 * Autonomous origin is a hard grouping boundary: a change in origin, or a
 * change in autonomous episode id, flushes the buffer. Foreground assistants
 * (neither side has origin) keep the current merge behavior.
 *
 * Exported for tests.
 */
export function mergeConsecutiveAssistantTurns(
  items: ThreadRenderItem[],
  mergeCache?: MergedAssistantRunCache
): ThreadRenderItem[] {
  const result: ThreadRenderItem[] = []
  const skipped: ThreadRenderItem[] = []
  let buffer: AssistantTurnItem[] = []

  // Push the cached merged item instead of rebuilding when the run's
  // membership (group references + item keys) is unchanged since last render.
  const reuseCachedMergedRun = (): boolean => {
    if (!mergeCache) return false
    const cached = mergeCache.get(buffer[0].group)
    if (!cached || cached.memberGroups.length !== buffer.length) return false
    for (let i = 0; i < buffer.length; i++) {
      if (
        buffer[i].group !== cached.memberGroups[i] ||
        buffer[i].key !== cached.memberKeys[i]
      ) {
        return false
      }
    }
    result.push(cached.item)
    return true
  }

  const flush = () => {
    if (buffer.length === 0) {
      // Drain any skipped (empty) items collected since last flush
      for (const s of skipped) result.push(s)
      skipped.length = 0
      return
    }

    if (buffer.length === 1) {
      result.push(buffer[0])
    } else if (reuseCachedMergedRun()) {
      // Reused — nothing to rebuild.
    } else {
      const allParts = buffer.flatMap((it) => it.group.parts)
      // A goal run straddling these merged sub-turns is still live only if the
      // final sub-turn is streaming; once it settles (stop / turn end / reload)
      // the unfinished-run shimmer must stop. Mirror groupGoalRuns' per-turn
      // isStreaming gate at the merge layer.
      const mergedStreaming = buffer.some((it) => it.phase === "streaming")
      // Fold tool-groups straddling the turn boundary, then collapse runs of
      // single-poll delegation-status and background-task groups (each polling
      // round is its own turn) into one merged card.
      const mergedParts = groupGoalRuns(
        mergeAdjacentBackgroundTaskGroups(
          mergeAdjacentDelegationStatusGroups(mergeAdjacentToolGroups(allParts))
        ),
        mergedStreaming
      )
      const last = buffer[buffer.length - 1]
      const first = buffer[0]

      // Union source-assistant text identities across sub-turns so eligibility
      // survives display-role merges (tool text identities stay out).
      const mergedAutolinkableTextParts = new Set<AutolinkableTextPart>()
      for (const item of buffer) {
        for (const part of item.group.autolinkableTextParts) {
          mergedAutolinkableTextParts.add(part)
        }
      }

      // Aggregate stats across the merged sub-turns so the post-stream
      // stats row reflects the whole assistant response, not just the
      // last sub-turn. Without this, multi-turn agents (Task tool, codex
      // agent loops, etc.) would visibly under-report tokens.
      let mergedUsage: import("@/lib/types").TurnUsage | null = null
      let mergedDuration: number | null = null
      let mergedGenerationMs: number | null = null
      let mergedGenerationTokens: number | null = null
      // Post-turn metadata may land on ANY sub-turn (Cursor's reparse patches
      // the FIRST local sub-turn when the parser emits fewer turns than the
      // live stream split into), so the merged completion time is the latest
      // non-null across the run — not whatever the last sub-turn happens to
      // carry.
      let mergedCompletedAt: string | null = null
      // Last non-null outcome wins so a trailing outcome-only sub-turn places
      // the interruption footer on the merged response group.
      let mergedOutcome: TurnOutcome | null | undefined
      const seenModels = new Set<string>()
      const mergedModels: string[] = []
      const seenReasoningEfforts = new Set<string>()
      const mergedReasoningEfforts: string[] = []
      for (const it of buffer) {
        if (it.group.completed_at) {
          mergedCompletedAt = it.group.completed_at
        }
        if (it.group.outcome) {
          mergedOutcome = it.group.outcome
        }
        const u = it.group.usage
        if (u) {
          if (!mergedUsage) {
            mergedUsage = {
              input_tokens: u.input_tokens,
              output_tokens: u.output_tokens,
              cache_creation_input_tokens: u.cache_creation_input_tokens,
              cache_read_input_tokens: u.cache_read_input_tokens,
            }
          } else {
            mergedUsage.input_tokens += u.input_tokens
            mergedUsage.output_tokens += u.output_tokens
            mergedUsage.cache_creation_input_tokens +=
              u.cache_creation_input_tokens
            mergedUsage.cache_read_input_tokens += u.cache_read_input_tokens
          }
        }
        if (typeof it.group.duration_ms === "number") {
          mergedDuration = (mergedDuration ?? 0) + it.group.duration_ms
        }
        if (typeof it.group.generation_ms === "number") {
          mergedGenerationMs =
            (mergedGenerationMs ?? 0) + it.group.generation_ms
        }
        if (typeof it.group.generation_tokens === "number") {
          mergedGenerationTokens =
            (mergedGenerationTokens ?? 0) + it.group.generation_tokens
        }
        if (it.group.model && !seenModels.has(it.group.model)) {
          seenModels.add(it.group.model)
          mergedModels.push(it.group.model)
        }
        const effort = it.group.reasoning_effort?.trim()
        if (effort && !seenReasoningEfforts.has(effort)) {
          seenReasoningEfforts.add(effort)
          mergedReasoningEfforts.push(effort)
        }
      }

      const merged: AssistantTurnItem = {
        ...last,
        key: `merged-${first.key}`,
        // Concatenate every sub-turn's raw turns so the artifacts card sees all
        // file edits across the merged reply, not just the last sub-turn.
        sourceTurns: buffer.flatMap((b) => b.sourceTurns),
        group: {
          ...last.group,
          id: first.group.id,
          autonomous_origin:
            first.group.autonomous_origin ?? last.group.autonomous_origin,
          parts: mergedParts,
          autolinkableTextParts:
            mergedAutolinkableTextParts.size > 0
              ? mergedAutolinkableTextParts
              : EMPTY_AUTOLINKABLE_TEXT_PARTS,
          usage: mergedUsage,
          duration_ms: mergedDuration,
          generation_ms: mergedGenerationMs,
          generation_tokens: mergedGenerationTokens,
          model: mergedModels[0] ?? last.group.model,
          models: mergedModels.length > 1 ? mergedModels : undefined,
          reasoning_effort:
            mergedReasoningEfforts[0] ?? last.group.reasoning_effort,
          reasoning_efforts:
            mergedReasoningEfforts.length > 1
              ? mergedReasoningEfforts
              : undefined,
          completed_at: mergedCompletedAt,
          outcome: mergedOutcome,
        },
      }
      result.push(merged)
      mergeCache?.set(first.group, {
        memberGroups: buffer.map((it) => it.group),
        memberKeys: buffer.map((it) => it.key),
        item: merged,
      })
    }

    // Drop any empty items that were collapsed inside the run
    skipped.length = 0
    buffer = []
  }

  for (const item of items) {
    if (item.kind === "turn" && item.group.role === "assistant") {
      // Flush any leading skipped (empty non-assistant) items before starting
      // a fresh assistant run. This keeps non-assistant placeholders in their
      // original relative order when no merging happens.
      if (buffer.length === 0) {
        for (const s of skipped) result.push(s)
        skipped.length = 0
      } else if (
        shouldFlushAutonomousAssistantRun(buffer[buffer.length - 1], item)
      ) {
        flush()
      }
      buffer.push(item)
      continue
    }

    if (buffer.length > 0 && isEmptyTurnItem(item)) {
      // Transparent: don't break the run, but track in case we end up not
      // merging (single-buffer case still drops them as they're invisible).
      skipped.push(item)
      continue
    }

    flush()
    result.push(item)
  }
  flush()

  return result
}

const UserMessageCopyButton = memo(function UserMessageCopyButton({
  parts,
}: {
  parts: AdaptedContentPart[]
}) {
  const t = useTranslations("Folder.chat.messageList")
  const [isCopied, setIsCopied] = useState(false)
  const timeoutRef = useRef<number>(0)

  const handleCopy = useCallback(async () => {
    if (isCopied) return
    // User text was Markdown-escaped by the composer on send (e.g. a Windows
    // path `C:\…` became `C:\\…`); the transcript renders it back through a
    // Markdown renderer, so the copy must reverse that escaping to match what
    // the user sees. Assistant copies (TurnStats below) keep the raw Markdown.
    const text = unescapeComposerText(extractTextFromParts(parts))
    if (!text) return
    const ok = await copyTextToClipboard(text)
    if (!ok) return
    setIsCopied(true)
    timeoutRef.current = window.setTimeout(() => setIsCopied(false), 2000)
  }, [parts, isCopied])

  useEffect(
    () => () => {
      window.clearTimeout(timeoutRef.current)
    },
    []
  )

  return (
    <MessageAction
      tooltip={isCopied ? t("copied") : t("copyMessage")}
      className="opacity-0 group-hover/user-msg:opacity-100 transition-opacity self-end"
      onClick={handleCopy}
      size="icon-xs"
    >
      {isCopied ? <CheckIcon size={12} /> : <CopyIcon size={12} />}
    </MessageAction>
  )
})

const UserMessageTaskButton = memo(function UserMessageTaskButton({
  parts,
}: {
  parts: AdaptedContentPart[]
}) {
  const t = useTranslations("Tasks")
  const getText = useCallback(
    () => unescapeComposerText(extractTextFromParts(parts)),
    [parts]
  )
  const createTask = useCreateTaskFromMessage(getText)
  return (
    <MessageAction
      tooltip={t("createFromMessage")}
      className="opacity-0 group-hover/user-msg:opacity-100 transition-opacity self-end"
      onClick={createTask}
      size="icon-xs"
    >
      <ListTodo size={12} />
    </MessageAction>
  )
})

const HistoricalMessageGroup = memo(function HistoricalMessageGroup({
  group,
  parentConversationId,
  dimmed = false,
  showStats = true,
  previousUserIndex = null,
  isResponseComplete = true,
  sourceTurns,
  renderKind = "historicalRow",
  showThinking = true,
  agentType,
}: {
  group: ResolvedMessageGroup
  parentConversationId?: number | null
  dimmed?: boolean
  showStats?: boolean
  previousUserIndex?: number | null
  isResponseComplete?: boolean
  sourceTurns?: MessageTurn[]
  renderKind?: "historicalRow" | "liveRow"
  showThinking?: boolean
  agentType: AgentType
}) {
  streamingPerfRecorder.countRender(renderKind)
  const t = useTranslations("Folder.chat.messageList")
  if (group.role === "system") {
    return (
      <CollapsibleSystemMessage group={group} showThinking={showThinking} />
    )
  }

  const hasBody =
    group.parts.length > 0 ||
    group.resources.length > 0 ||
    group.images.length > 0
  const showInterruptedFooter =
    group.role === "assistant" && group.outcome?.status === "interrupted"

  return (
    <div className={dimmed ? "opacity-70" : undefined}>
      {group.role === "assistant" && group.autonomous_origin ? (
        <div
          data-testid="background-continuation-marker"
          className="mb-1.5 flex items-center gap-1.5 text-xs text-muted-foreground"
        >
          <History aria-hidden="true" className="h-3.5 w-3.5 shrink-0" />
          <span>{t("backgroundContinuation")}</span>
        </div>
      ) : null}
      {/* Outcome-only turns keep the footer but suppress an empty bubble. */}
      {hasBody || group.role === "user" ? (
        <Message from={group.role}>
          {group.role === "user" && group.images.length > 0 ? (
            <UserImageAttachments images={group.images} className="self-end" />
          ) : null}
          {group.role === "user" ? (
            <div className="group/user-msg flex w-fit ml-auto max-w-full items-start gap-1">
              <UserMessageTaskButton parts={group.parts} />
              <UserMessageCopyButton parts={group.parts} />
              <MessageContent>
                <CollapsibleUserMessage
                  parts={group.parts}
                  parentConversationId={parentConversationId}
                  showThinking={showThinking}
                />
              </MessageContent>
            </div>
          ) : (
            <MessageContent>
              <ContentPartsRenderer
                parts={group.parts}
                role={group.role}
                parentConversationId={parentConversationId}
                autolinkLocalPathParts={
                  isResponseComplete ? group.autolinkableTextParts : undefined
                }
                showThinking={showThinking}
              />
            </MessageContent>
          )}
          {group.role === "user" && group.resources.length > 0 ? (
            <UserResourceLinks
              resources={group.resources}
              className="self-end"
            />
          ) : null}
        </Message>
      ) : null}
      {showInterruptedFooter ? (
        <div
          data-testid="response-interrupted-footer"
          className="mt-2 -ms-[0.3125rem] flex items-center gap-1.5 text-xs text-muted-foreground"
        >
          <CircleStop aria-hidden="true" className="h-3.5 w-3.5 shrink-0" />
          <span>{t("responseInterrupted")}</span>
        </div>
      ) : null}
      {showStats && group.role === "assistant" && sourceTurns && (
        <ReplyArtifacts
          sourceTurns={sourceTurns}
          isResponseComplete={isResponseComplete}
        />
      )}
      {showStats && group.role === "assistant" && (
        <TurnStats
          usage={group.usage}
          duration_ms={group.duration_ms}
          generationMs={group.generation_ms}
          generationTokens={group.generation_tokens}
          agentType={agentType}
          model={group.model}
          models={group.models}
          reasoningEffort={group.reasoning_effort}
          reasoningEfforts={group.reasoning_efforts}
          previousUserIndex={previousUserIndex}
          isResponseComplete={isResponseComplete}
          copyText={extractTextFromParts(group.parts)}
          completedAt={group.completed_at}
        />
      )}
    </div>
  )
})

const PendingTypingIndicator = memo(function PendingTypingIndicator() {
  return (
    <Message from="assistant">
      <MessageContent>
        <div className="flex items-center gap-1.5 py-1">
          <span className="inline-block h-1.5 w-1.5 rounded-full bg-muted-foreground/60 animate-[pulse_1.4s_ease-in-out_infinite]" />
          <span className="inline-block h-1.5 w-1.5 rounded-full bg-muted-foreground/60 animate-[pulse_1.4s_ease-in-out_0.2s_infinite]" />
          <span className="inline-block h-1.5 w-1.5 rounded-full bg-muted-foreground/60 animate-[pulse_1.4s_ease-in-out_0.4s_infinite]" />
        </div>
      </MessageContent>
    </Message>
  )
})

function waitingPlaceholderLiveMessage(startedAtMs: number): LiveMessage {
  return {
    id: "waiting-for-subagents",
    role: "assistant",
    content: [],
    startedAt: startedAtMs,
  }
}

const AutoScrollOnSend = memo(function AutoScrollOnSend({
  signal,
}: {
  signal: number
}) {
  const { scrollToBottom } = useStickToBottomContext()
  const lastSignalRef = useRef(signal)

  useEffect(() => {
    if (signal === lastSignalRef.current) return
    lastSignalRef.current = signal

    scrollToBottom()
    const rafId = requestAnimationFrame(() => {
      scrollToBottom()
    })
    return () => {
      cancelAnimationFrame(rafId)
    }
  }, [scrollToBottom, signal])

  return null
})

/**
 * Build a UI-only LiveMessage projection from the live transcript snapshot so
 * plan/stats overlays can reuse existing components without MessageListView
 * subscribing to live content (keeps historicalThread cold).
 */
function liveSnapshotToLiveMessage(snap: LiveTranscriptSnapshot): LiveMessage {
  const content: LiveContentBlock[] = []
  for (const id of snap.segmentIds) {
    const segment = snap.segments.get(id)
    if (!segment) continue
    switch (segment.type) {
      case "text":
        content.push({ type: "text", text: segment.text })
        break
      case "thinking":
        content.push({ type: "thinking", text: segment.text })
        break
      case "plan":
        content.push({ type: "plan", entries: segment.entries })
        break
      case "tool":
      case "generated-image": {
        const tool = snap.tools.get(segment.toolCallId)
        if (tool) content.push({ type: "tool_call", info: tool })
        break
      }
    }
  }
  return {
    id: snap.messageId,
    role: "assistant",
    content,
    startedAt: snap.startedAt,
  }
}

function extractLiveDelegationSources(
  message: LiveMessage,
  parentConversationId: number
): DelegationCardSource[] {
  const liveSources: DelegationCardSource[] = []
  for (const block of message.content) {
    if (block.type !== "tool_call") continue
    const toolName = normalizeToolName(
      inferLiveToolName({
        title: block.info.title,
        kind: block.info.kind,
        rawInput: block.info.raw_input,
        meta: block.info.meta,
        owner: block.info,
      })
    )
    if (!isDelegateToAgentToolName(toolName)) continue
    const resolvedOutput =
      block.info.raw_output_chunks.length > 0
        ? block.info.raw_output_chunks.join("")
        : block.info.content
    liveSources.push({
      parentToolUseId: block.info.tool_call_id,
      parentConversationId,
      input: block.info.raw_input ?? null,
      output: resolvedOutput,
      errorText:
        block.info.status === "failed" ? (resolvedOutput ?? null) : null,
      state:
        block.info.status === "completed"
          ? "output-available"
          : block.info.status === "failed"
            ? "output-error"
            : "input-available",
      meta: block.info.meta ?? null,
    })
  }
  return liveSources
}

const EMPTY_ACTIVITIES: DelegationActivityView[] = []

/**
 * Bottom live-turn banner. While streaming, tracks the live transcript.
 * While waiting for subagents, keeps the same layout but swaps the status
 * label; metrics come from the latched pre-suspend live message when
 * available, otherwise a timer-only placeholder from `armed_at`.
 */
const LiveTurnStatsBanner = memo(function LiveTurnStatsBanner({
  conversationId,
  agentType,
  isStreaming,
  isWaitingForSubagents,
  waitingArmedAtMs,
}: {
  conversationId: number
  agentType: AgentType
  isStreaming: boolean
  isWaitingForSubagents: boolean
  waitingArmedAtMs: number | null
}) {
  const snap = useLiveTranscriptConversation(conversationId)
  const liveMessage = useMemo(
    () => (snap ? liveSnapshotToLiveMessage(snap) : null),
    [snap]
  )
  // Compatibility path may still publish session.liveMessage without incremental
  // transcript; keep it as a latch source.
  const runtimeLiveMessage = useConversationRuntimeStore(
    (s) => s.byConversationId.get(conversationId)?.liveMessage ?? null
  )
  const activeLive = liveMessage ?? runtimeLiveMessage
  const [latchedLive, setLatchedLive] = useState<LiveMessage | null>(null)

  useEffect(() => {
    // Preserve the last live metrics when the runtime clears them before waiting.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    if (activeLive) setLatchedLive(activeLive)
  }, [activeLive])

  useEffect(() => {
    if (!isWaitingForSubagents && !isStreaming) {
      // Reset only after both live display modes have ended.
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setLatchedLive(null)
    }
  }, [isWaitingForSubagents, isStreaming])

  if (isWaitingForSubagents) {
    const startedAt =
      activeLive?.startedAt ?? latchedLive?.startedAt ?? waitingArmedAtMs
    if (startedAt == null) return null
    const message =
      activeLive ?? latchedLive ?? waitingPlaceholderLiveMessage(startedAt)
    return (
      <LiveTurnStats
        message={message}
        agentType={agentType}
        conversationId={conversationId}
        isStreaming
        statusMode="waiting_for_subagents"
      />
    )
  }

  if (isStreaming && activeLive) {
    return (
      <LiveTurnStats
        message={activeLive}
        agentType={agentType}
        conversationId={conversationId}
        isStreaming
        statusMode="auto"
      />
    )
  }

  return null
})

/** Narrow-subscription plan overlay driven by live transcript segments. */
const LiveAgentPlanOverlay = memo(function LiveAgentPlanOverlay({
  conversationId,
  entries,
  planKey,
  isStreaming,
}: {
  conversationId: number
  entries: ReturnType<typeof extractLatestPlanEntriesFromMessages>
  planKey: string | null
  isStreaming: boolean
}) {
  const snap = useLiveTranscriptConversation(conversationId)
  const message = useMemo(
    () => (snap ? liveSnapshotToLiveMessage(snap) : null),
    [snap]
  )
  return (
    <AgentPlanOverlay
      key={message?.id != null ? `plan-${message.id}` : (planKey ?? undefined)}
      message={message}
      entries={entries}
      planKey={planKey}
      defaultExpanded={false}
      isStreaming={isStreaming}
    />
  )
})

/**
 * Sub-agent overlay: full conversation history, with live-transcript rows
 * preferred for the in-flight turn so status updates without waiting on
 * historical adaptation. Native activity is derived alongside and never
 * replaces original tool rendering.
 */
const LiveAwareSubAgentOverlay = memo(function LiveAwareSubAgentOverlay({
  conversationId,
  durableConversationId,
  agentType,
  isStreaming,
  historicalDelegations,
  historicalActivities,
  historicalKey,
  workspaceRootPath,
  isActive,
  onResumeRoot,
  onOpenRootConversation,
}: {
  conversationId: number
  durableConversationId: number
  agentType: AgentType
  isStreaming: boolean
  historicalDelegations: DelegationCardSource[]
  /**
   * Store + full-session historical composition from the parent. Live
   * projection always merges on top while streaming (never gated on store
   * non-emptiness).
   */
  historicalActivities: DelegationActivityView[]
  historicalKey: string
  workspaceRootPath: string | null
  isActive: boolean
  onResumeRoot?: () => void | Promise<void>
  onOpenRootConversation?: (conversationId: number) => void | Promise<void>
}) {
  const snap = useLiveTranscriptConversation(
    isStreaming ? conversationId : null
  )
  const liveDelegations = useMemo(() => {
    if (!snap || !isStreaming) return EMPTY_DELEGATIONS
    return extractLiveDelegationSources(
      liveSnapshotToLiveMessage(snap),
      conversationId
    )
  }, [snap, isStreaming, conversationId])
  // Always project live natives while streaming; merge/dedupe with the
  // parent store+historical set (deterministic by task_id / precedence rules).
  const liveActivities = useMemo(() => {
    if (!snap || !isStreaming) return EMPTY_ACTIVITIES
    return projectNativeActivitiesFromTranscript(snap, agentType)
  }, [snap, isStreaming, agentType])
  const activities = useMemo(() => {
    if (liveActivities.length > 0) {
      return dedupeDelegationActivities(historicalActivities, liveActivities)
    }
    return historicalActivities
  }, [liveActivities, historicalActivities])
  // Full-session historical rows + fresher live transcript rows for in-flight
  // turns. Live wins on tool / child / task identity.
  const delegations = useMemo(() => {
    if (liveDelegations.length === 0) return historicalDelegations
    return mergeDelegationSourceLayers(historicalDelegations, liveDelegations)
  }, [historicalDelegations, liveDelegations])
  // Conversation-scoped key so expand/collapse survives new turns and the
  // live↔historical handoff (unlike a per-message key which remounts the panel).
  const workflowGraph = useConversationRuntimeStore(
    (s) => s.byConversationId.get(conversationId)?.detail?.workflow_graph
  )

  return (
    <SubAgentOverlay
      key={historicalKey}
      delegations={delegations}
      activities={activities}
      overlayKey={historicalKey}
      defaultExpanded
      conversationId={durableConversationId}
      workflowGraph={workflowGraph}
      workspaceRootPath={workspaceRootPath}
      isActive={isActive}
      onResumeRoot={onResumeRoot}
      onOpenRootConversation={onOpenRootConversation}
    />
  )
})

export function MessageListView({
  conversationId,
  agentType,
  workspaceRootPath = null,
  connStatus,
  isActive = true,
  sendSignal = 0,
  detailLoading = false,
  detailError = null,
  acpLoadError = null,
  acpLoadErrorCode = null,
  hideEmptyState = false,
  onReload,
  onNewSession,
  showMessageNav = true,
  initialHistoryScrollEligible = false,
  historyLoadComplete = false,
  focusTurnAnchor = null,
  waitingForSubagentsArmedAtMs = null,
  onResumeRoot,
  onOpenRootConversation,
  userTurnHeader = null,
}: MessageListViewProps) {
  const isWaitingForSubagents =
    waitingForSubagentsArmedAtMs != null &&
    Number.isFinite(waitingForSubagentsArmedAtMs)
  const t = useTranslations("Folder.chat.messageList")
  const sharedT = useTranslations("Folder.chat.shared")
  const durableConversationId = useConversationRuntimeStore(
    useCallback(
      (s) =>
        s.byConversationId.get(conversationId)?.dbConversationId ??
        conversationId,
      [conversationId]
    )
  )
  const durableDelegationSources = useDurableDelegationSources(
    durableConversationId
  )
  const useIncrementalLive = useStreamingPerformanceFlag(
    "incremental_live_transcript"
  )
  const showThinking = useAgentThinkingVisibility(agentType)
  const historyWindow = useConversationRuntimeStore(
    useCallback(
      (s) =>
        s.byConversationId.get(conversationId)?.detail?.history_window ?? null,
      [conversationId]
    )
  )
  const detailHistoryLoadingOlder = useConversationRuntimeStore(
    useCallback(
      (s) =>
        s.byConversationId.get(conversationId)?.detailHistoryLoadingOlder ??
        false,
      [conversationId]
    )
  )
  const loadOlderHistory = useConversationRuntimeStore(
    (s) => s.actions.loadOlderHistory
  )
  const onLoadOlderHistory = useCallback(() => {
    loadOlderHistory(conversationId)
  }, [conversationId, loadOlderHistory])
  const { loadOlderTurns } = useConversationRuntimeActions()
  // Narrow selectors: the whole session object changes on live tokens, and
  // subscribing to it would re-render the historical thread during streaming.
  const olderTurnsPrependEpoch = useConversationRuntimeStore(
    (s) => s.byConversationId.get(conversationId)?.olderTurnsPrependEpoch ?? 0
  )
  const sessionLoadingOlderTurns = useConversationRuntimeStore((s) =>
    Boolean(s.byConversationId.get(conversationId)?.loadingOlderTurns)
  )
  const sessionTurnsOffset = useConversationRuntimeStore((s) => {
    const detail = s.byConversationId.get(conversationId)?.detail ?? null
    if (!isWindowedDetail(detail)) return 0
    return detail.turns_offset ?? 0
  })
  const hasOlderTurns =
    Boolean(historyWindow?.has_more_before) || sessionTurnsOffset > 0
  const loadingOlderTurns =
    detailHistoryLoadingOlder || sessionLoadingOlderTurns
  const handleLoadOlder = useCallback(() => {
    if (historyWindow?.has_more_before) {
      onLoadOlderHistory()
      return
    }
    loadOlderTurns(conversationId)
  }, [
    conversationId,
    historyWindow?.has_more_before,
    loadOlderTurns,
    onLoadOlderHistory,
  ])

  // One-shot latch: initialized once from mount-time eligibility; only the
  // controller clears it. Later prop changes never re-arm this state.
  const [initialHistoryScrollPending, setInitialHistoryScrollPending] =
    useState(() => initialHistoryScrollEligible && !focusTurnAnchor)
  const initialHistoryScrollActive =
    initialHistoryScrollPending && !focusTurnAnchor
  const focusedTurnAnchorRef = useRef<string | null>(null)
  // Updated each render after threadItems is built; finish reads the latest.
  const lastHistoryIndexRef = useRef(0)
  // Declared early so the finish callback can close over it; VirtualizedMessageThread
  // publishes into this after mount.
  const scrollApiRef = useRef<MessageScrollContextValue | null>(null)
  const finishInitialHistoryScroll = useCallback(() => {
    setInitialHistoryScrollPending(false)
    // Protective: virtua may still hold a stale offset after stick-to-bottom's
    // programmatic jump. Align to the last history row without changing the
    // intended "open at bottom" placement. Re-read the ref inside rAF so a
    // publish that races finish still works.
    const index = lastHistoryIndexRef.current
    if (index < 0) return
    requestAnimationFrame(() => {
      scrollApiRef.current?.scrollToIndex(index, { align: "end" })
    })
  }, [])

  // Compatibility `selectTimelineTurns` allocates a new outer array whenever a
  // live message is present. Zustand v5's getSnapshot must return a stable
  // reference across consecutive reads or React 19 infinite-loops. Cache the
  // result by the stable historical array + liveMessage object identity.
  const compatibilityTimelineCacheRef = useRef<{
    conversationId: number
    historical: ReturnType<typeof selectHistoricalTimelineTurns>
    liveMessage: LiveMessage | null
    result: ReturnType<typeof selectTimelineTurns>
  } | null>(null)

  // When incremental live is on: historical timeline only (reference-stable
  // across live content updates) + narrow syncState. Compatibility path keeps
  // the full timeline (incl. streaming phase) and liveMessage for overlays.
  const timelineTurns = useConversationRuntimeStore(
    useCallback(
      (s) => {
        if (useIncrementalLive) {
          return selectHistoricalTimelineTurns(s, conversationId)
        }
        const historical = selectHistoricalTimelineTurns(s, conversationId)
        const liveMessage =
          s.byConversationId.get(conversationId)?.liveMessage ?? null
        const cached = compatibilityTimelineCacheRef.current
        if (
          cached &&
          cached.conversationId === conversationId &&
          cached.historical === historical &&
          cached.liveMessage === liveMessage
        ) {
          return cached.result
        }
        const result = selectTimelineTurns(s, conversationId)
        compatibilityTimelineCacheRef.current = {
          conversationId,
          historical,
          liveMessage,
          result,
        }
        return result
      },
      [conversationId, useIncrementalLive]
    )
  )
  const sessionSyncState = useConversationRuntimeStore(
    useCallback(
      (s) => s.byConversationId.get(conversationId)?.syncState ?? "idle",
      [conversationId]
    )
  )
  const compatibilityLiveMessage = useConversationRuntimeStore(
    useCallback(
      (s) =>
        useIncrementalLive
          ? null
          : (s.byConversationId.get(conversationId)?.liveMessage ?? null),
      [conversationId, useIncrementalLive]
    )
  )
  const hasLiveTranscript = useHasLiveTranscript(
    useIncrementalLive ? conversationId : null
  )
  // Compatibility path only: incremental mode never reads session.liveMessage
  // so content-only SET_LIVE_MESSAGE updates cannot re-render this list.
  const liveMessage = compatibilityLiveMessage

  streamingPerfRecorder.countRender("historicalThread")

  // After React commit, drain pending deliveries and let the recorder schedule
  // a coalesced next-paint RAF. Paint scheduling lives on the recorder so
  // rapid re-render effect cleanup cannot cancel samples before paint.
  useLayoutEffect(() => {
    streamingPerfRecorder.markReactCommit()
  })

  const adapterText = useMemo(
    () => ({
      attachedResources: sharedT("attachedResources"),
      toolCallFailed: sharedT("toolCallFailed"),
    }),
    [sharedT]
  )

  // Per-instance turn adapter: caches per-turn `AdaptedMessage` so unchanged
  // historical turns survive every streaming-token re-render with stable refs.
  const [turnAdapter] = useState<MessageTurnAdapter>(() =>
    createMessageTurnAdapter()
  )

  // Sibling cache mapping each cached `AdaptedMessage` to its derived
  // `ResolvedMessageGroup`, so `HistoricalMessageGroup`'s `memo` can short-
  // circuit on prop reference equality.
  const [groupCache] = useState<WeakMap<AdaptedMessage, ResolvedMessageGroup>>(
    () => new WeakMap()
  )

  // Reuses merged multi-sub-turn assistant items across streaming-batch
  // re-renders — see MergedAssistantRunCacheEntry for the validity contract.
  const [mergedRunCache] = useState<MergedAssistantRunCache>(
    () => new WeakMap()
  )

  const adaptedThread = useMemo(() => {
    const allTurns = timelineTurns.map((item) => item.turn)
    const streamingIndices = new Set<number>()
    const inProgressToolCallIdsByIndex = new Map<number, Set<string>>()
    timelineTurns.forEach((item, i) => {
      if (item.phase === "streaming") streamingIndices.add(i)
      // Not gated on the streaming phase: a PERSISTED turn of a conversation
      // that is still running (viewer without the live stream) also carries
      // in-flight calls, marked by the store from the backend's
      // `in_flight_user_turn_id`. Both phases feed the same adapter knob.
      if (item.inProgressToolCallIds && item.inProgressToolCallIds.size > 0) {
        inProgressToolCallIdsByIndex.set(i, item.inProgressToolCallIds)
      }
    })
    const allAdapted = turnAdapter.adapt(
      allTurns,
      adapterText,
      streamingIndices.size > 0 ? streamingIndices : undefined,
      inProgressToolCallIdsByIndex.size > 0
        ? inProgressToolCallIdsByIndex
        : undefined
    )

    // Collect non-streaming adapted messages for plan extraction
    const nonStreaming = allAdapted.filter(
      (_, index) => timelineTurns[index].phase !== "streaming"
    )
    // Drop exact Codex interrupt markers on every session (parent + child).
    // The marker is a turn-abort fence, not a user-facing answer.
    const displayAdapted = allAdapted.map((message) => {
      if (message.role !== "assistant") return message
      const content = filterConversationInterruptedParts(message.content)
      return content === message.content ? message : { ...message, content }
    })
    const projected = projectDelegationTranscript(
      displayAdapted,
      conversationId
    )

    // Map each adapted message directly to a render item (1:1).
    // Backend group_into_turns() already ensures each turn is a complete unit.
    const rawItems: ThreadRenderItem[] = projected.messages.map((msg, i) => {
      const phase = timelineTurns[i].phase
      const role = msg.role === "tool" ? "assistant" : msg.role
      const autonomousOrigin = allTurns[i].autonomous_origin ?? undefined
      let group = groupCache.get(msg)
      if (!group || group.autonomous_origin !== autonomousOrigin) {
        group = {
          id: msg.id,
          role,
          parts: msg.content,
          resources: msg.userResources ?? [],
          images: msg.userImages ?? [],
          autolinkableTextParts: topLevelAssistantTextParts(msg),
          usage: msg.usage,
          duration_ms: msg.duration_ms,
          generation_ms: msg.generation_ms,
          generation_tokens: msg.generation_tokens,
          model: msg.model,
          reasoning_effort: msg.reasoning_effort,
          completed_at: msg.completed_at,
          outcome: msg.outcome,
          autonomous_origin: autonomousOrigin,
        }
        groupCache.set(msg, group)
      }
      // Include phase and role so a turn that briefly coexists across phases (e.g.
      // a streaming turn that has just been promoted to localTurns while the
      // liveMessage is still attached) doesn't collide with itself in the
      // virtualized list. Never include the array index: older-page prepends
      // must keep every existing Virtua key stable for scroll anchoring.
      const key = `${phase}-${role}-${msg.id}`
      // Hoist a compaction-only turn to its own standalone divider item so it
      // renders BETWEEN turns instead of being merged into (and wedged inside)
      // the preceding assistant reply by `mergeConsecutiveAssistantTurns`.
      const compactionMeta = compactionOnlyMeta(group)
      if (compactionMeta !== null) {
        return { key, kind: "compaction" as const, meta: compactionMeta }
      }
      return {
        key,
        kind: "turn" as const,
        group,
        phase,
        showStats: false,
        isRoleTransition: false,
        previousUserIndex: null,
        sourceTurns: singletonSourceTurns(allTurns[i]),
      }
    })

    // Collapse consecutive assistant turn render items into a single rendered
    // turn, so tool-groups straddling a turn boundary fold into one collapsible.
    const items = mergeConsecutiveAssistantTurns(rawItems, mergedRunCache)

    // Compute showStats, isRoleTransition, and previousUserIndex for each turn.
    // previousUserIndex points at the closest preceding user turn (used by the
    // post-stream stats row's "jump to previous user message" button).
    let lastUserIdx: number | null = null
    for (let idx = 0; idx < items.length; idx++) {
      const item = items[idx]
      if (item.kind !== "turn") continue

      // Reset before recomputing: a cached merged item carries last render's
      // values and the conditions below only ever assign `true`.
      item.showStats = false
      item.isRoleTransition = false
      item.previousUserIndex = null

      // isRoleTransition: role differs from previous turn item
      if (idx > 0) {
        const prev = items[idx - 1]
        if (prev.kind === "turn" && prev.group.role !== item.group.role) {
          item.isRoleTransition = true
        }
      }

      if (item.group.role === "user") {
        lastUserIdx = idx
      }

      // showStats: only on the last assistant turn before a non-assistant or end
      if (item.group.role === "assistant") {
        const next = items[idx + 1]
        if (!next || next.kind !== "turn" || next.group.role !== "assistant") {
          item.showStats = true
          item.previousUserIndex = lastUserIdx
        }
      }
    }

    // Pending typing is a footer concern under incremental live (outside
    // Virtua). Compatibility path keeps the typing virtua item.
    const lastPhase = timelineTurns[timelineTurns.length - 1]?.phase ?? null
    if (
      !useIncrementalLive &&
      lastPhase === "optimistic" &&
      (connStatus === "prompting" || sessionSyncState === "awaiting_persist")
    ) {
      items.push({ key: "pending-typing", kind: "typing" })
    }

    return {
      threadItems: items,
      nonStreamingAdapted: nonStreaming,
      delegationIdentityIndex: projected.identityIndex,
      delegationRunRecords: projected.runRecords,
    }
  }, [
    adapterText,
    connStatus,
    sessionSyncState,
    timelineTurns,
    turnAdapter,
    groupCache,
    useIncrementalLive,
    mergedRunCache,
    conversationId,
  ])
  const {
    threadItems,
    nonStreamingAdapted,
    delegationIdentityIndex,
    delegationRunRecords,
  } = adaptedThread

  const lastTimelinePhase =
    timelineTurns[timelineTurns.length - 1]?.phase ?? null
  const showPendingTypingFooter =
    useIncrementalLive &&
    lastTimelinePhase === "optimistic" &&
    (connStatus === "prompting" || sessionSyncState === "awaiting_persist")
  const showLiveFooter =
    useIncrementalLive && (hasLiveTranscript || showPendingTypingFooter)
  const liveFooter = useMemo(() => {
    if (!showLiveFooter) return null
    return (
      <LiveTranscriptRow
        conversationId={conversationId}
        agentType={agentType}
        showThinking={showThinking}
        delegationRunRecords={delegationRunRecords}
        delegationIdentityIndex={delegationIdentityIndex}
      />
    )
  }, [
    showLiveFooter,
    conversationId,
    agentType,
    showThinking,
    delegationRunRecords,
    delegationIdentityIndex,
  ])

  const historicalPlanEntries = useMemo(
    () => extractLatestPlanEntriesFromMessages(nonStreamingAdapted),
    [nonStreamingAdapted]
  )
  const historicalPlanKey = useMemo(
    () => buildPlanKey(historicalPlanEntries),
    [historicalPlanEntries]
  )

  const renderThreadItem = useCallback(
    (item: ThreadRenderItem) => {
      switch (item.kind) {
        case "turn": {
          const pt = item.isRoleTransition ? 16 : 0
          const phaseLabel =
            item.group.role === "user" && userTurnHeader
              ? userTurnHeader(item.group)
              : null
          return (
            <div style={pt > 0 ? { paddingTop: pt } : undefined}>
              {phaseLabel ? (
                <div className="flex items-center gap-2 px-1 pb-3 pt-1">
                  <span aria-hidden="true" className="h-px flex-1 bg-border" />
                  <span className="shrink-0 rounded-full border border-border bg-muted/50 px-2 py-0.5 text-[0.625rem] font-medium leading-none text-muted-foreground">
                    {phaseLabel}
                  </span>
                  <span aria-hidden="true" className="h-px flex-1 bg-border" />
                </div>
              ) : null}
              <HistoricalMessageGroup
                group={item.group}
                parentConversationId={conversationId}
                dimmed={item.phase === "optimistic"}
                showStats={item.showStats}
                previousUserIndex={item.previousUserIndex}
                isResponseComplete={item.phase === "persisted"}
                sourceTurns={item.sourceTurns}
                renderKind={
                  item.phase === "streaming" ? "liveRow" : "historicalRow"
                }
                showThinking={showThinking}
                agentType={agentType}
              />
            </div>
          )
        }
        case "typing":
          return <PendingTypingIndicator />
        case "compaction":
          // Chrome-less centered divider between turns (no avatar / stats footer).
          return (
            <div className="px-1 py-2">
              <ContextCompactionCard meta={item.meta} />
            </div>
          )
        default:
          return null
      }
    },
    [showThinking, conversationId, userTurnHeader]
  )

  const emptyState = useMemo(
    () =>
      hideEmptyState ? null : (
        <div className="px-4 py-12 text-center">
          <p className="text-muted-foreground text-sm">
            {t("emptyConversation")}
          </p>
        </div>
      ),
    [hideEmptyState, t]
  )

  // Namespaced with `plan-` so this key can never equal `subAgentOverlayKey`
  // below: the two overlays are siblings in one container, and both fall back
  // to a per-conversation string when there's no live message / assistant reply
  // yet (the state a freshly-opened sub-agent dialog starts in). Without
  // disjoint namespaces those fallbacks collide → React "two children with the
  // same key".
  const agentPlanOverlayKey =
    liveMessage?.id != null
      ? `plan-${liveMessage.id}`
      : `plan-history-${conversationId}`

  // Transcript walk first, then durable children under it. Compaction can drop
  // delegate cards from the visible tail; `list_child_conversations` still has
  // them. Transcript / continue_delegation rows win on tool / child / task id.
  const transcriptDelegations = useMemo(() => {
    const out: DelegationCardSource[] = []
    for (const item of threadItems) {
      if (item.kind === "turn" && item.group.role === "assistant") {
        collectDelegationSources(item.group.parts, out, conversationId)
      }
    }
    return out.length > 0 ? out : EMPTY_DELEGATIONS
  }, [threadItems, conversationId])
  const allSessionDelegations = useMemo(() => {
    if (durableDelegationSources.length === 0) return transcriptDelegations
    const merged = mergeDelegationSourceLayers(
      durableDelegationSources,
      transcriptDelegations
    )
    return merged.length > 0 ? merged : EMPTY_DELEGATIONS
  }, [durableDelegationSources, transcriptDelegations])
  // Store-backed activities (COMPLETE_TURN / SET_LIVE_MESSAGE / detail fetch).
  // Stable empty reference when absent — required for Zustand getSnapshot.
  // Store is last-assistant-only by design; always merge with full-session
  // historical derivation below (never short-circuit on store non-emptiness).
  const workflowGraph = useConversationRuntimeStore(
    (s) => s.byConversationId.get(conversationId)?.detail?.workflow_graph
  )

  const storeActivities = useConversationRuntimeStore((s) =>
    selectDelegationActivities(s, conversationId)
  )
  // Full-session walk of adapted parts — including background-task-group polls
  // so Claude TaskOutput/TaskStop still project after adapter grouping (I2).
  // Merge/dedupe with store materialization so prior-turn native rows survive
  // when the store only holds the latest assistant turn.
  const sessionActivities = useMemo(() => {
    const tools: Array<{
      toolCallId: string
      toolName: string
      input?: string | null
      output?: string | null
      status?: string | null
      meta?: Record<string, unknown> | null
    }> = []
    const walk = (parts: readonly AdaptedContentPart[]) => {
      for (const part of parts) {
        if (part.type === "tool-call" && part.toolCallId) {
          tools.push({
            toolCallId: part.toolCallId,
            toolName: part.toolName,
            input: part.input ?? null,
            output: part.output ?? part.errorText ?? null,
            status:
              part.state === "output-error"
                ? "failed"
                : part.state === "output-available"
                  ? "completed"
                  : "in_progress",
            meta: part.meta ?? null,
          })
        } else if (part.type === "delegation-work-unit") {
          walk(part.sources)
        } else if (part.type === "tool-group") {
          walk(part.items)
        } else if (part.type === "goal-run") {
          walk(part.items)
        } else if (part.type === "background-task-group") {
          // Historical Claude TaskOutput/TaskStop are grouped; walk polls
          // without flattening/replacing the original background-task card.
          for (const poll of part.polls) {
            if (!poll.toolCallId) continue
            tools.push({
              toolCallId: poll.toolCallId,
              toolName: poll.toolName,
              input: poll.input ?? null,
              output: poll.output ?? poll.errorText ?? null,
              status:
                poll.state === "output-error"
                  ? "failed"
                  : poll.state === "output-available"
                    ? "completed"
                    : "in_progress",
              meta: poll.meta ?? null,
            })
          }
        }
      }
    }
    for (const item of threadItems) {
      if (item.kind === "turn" && item.group.role === "assistant") {
        walk(item.group.parts)
      }
    }
    const derived = deriveNativeActivitiesFromToolCalls(tools, agentType)
    if (storeActivities.length === 0) {
      return derived.length === 0 ? EMPTY_ACTIVITIES : derived
    }
    if (derived.length === 0) {
      return storeActivities
    }
    return dedupeDelegationActivities(storeActivities, derived)
  }, [threadItems, agentType, storeActivities])
  // Conversation-scoped so expand/collapse is retained across turns.
  const subAgentOverlayKey = `subagents-${conversationId}`

  // --- Message navigator panel ------------------------------------------------
  // scrollApiRef is declared near initial-history finish (above) so that path
  // can re-align virtua after open-history placement.
  // Collapse state is owned here (not in the panel) so the expensive per-file
  // `navEntries` is computed only while the panel is open.
  const [navExpanded, setNavExpanded] = useState(false)

  // Cheap user-message tally for the collapsed chip — counts user turns without
  // parsing any file diffs.
  const userMessageCount = useMemo(() => {
    if (!showMessageNav) return 0
    let count = 0
    for (const item of threadItems) {
      if (item.kind === "turn" && item.group.role === "user") count += 1
    }
    return count
  }, [showMessageNav, threadItems])

  // One entry per user message — including ones with no edits (placeholders).
  // Computed lazily: only while the panel is expanded, since
  // `extractSessionFilesGrouped` parses every turn's diffs. Collapsed (the
  // default) it stays EMPTY, keeping the streaming hot path free of diff parsing.
  //
  // Windowed loading caveat (accepted degradation): counts, ordinals and file
  // summaries cover only the LOADED window — paging in older history extends
  // them. Nav targets are recomputed with the items on every prepend, so the
  // indices themselves never go stale.
  const navEntries = useMemo<MessageNavEntry[]>(() => {
    if (!showMessageNav || !navExpanded) return EMPTY_NAV_ENTRIES
    const turns = timelineTurns.map((item) => item.turn)
    const groups = extractSessionFilesGrouped(turns, { includeEmpty: true })
    if (groups.length === 0) return EMPTY_NAV_ENTRIES

    const indexByTurnId = new Map<string, number>()
    for (let i = 0; i < threadItems.length; i++) {
      const item = threadItems[i]
      if (item.kind === "turn" && item.group.role === "user") {
        indexByTurnId.set(item.group.id, i)
      }
    }

    const entries: MessageNavEntry[] = []
    for (const group of groups) {
      const threadIndex = indexByTurnId.get(group.userTurnId)
      if (threadIndex == null) continue
      let additions = 0
      let deletions = 0
      for (const file of group.files) {
        additions += file.additions
        deletions += file.deletions
      }
      entries.push({
        threadIndex,
        turnId: group.userTurnId,
        ordinal: entries.length + 1,
        label: group.userMessage,
        additions,
        deletions,
        files: group.files,
        hasChanges: group.files.length > 0,
      })
    }
    return entries.length > 0 ? entries : EMPTY_NAV_ENTRIES
  }, [showMessageNav, navExpanded, timelineTurns, threadItems])

  // -1 when empty so finish does not call scrollToIndex on a vacant virtua.
  // Keep this out of render so react-hooks/refs does not treat it as a side effect.
  useLayoutEffect(() => {
    lastHistoryIndexRef.current =
      threadItems.length > 0 ? threadItems.length - 1 : -1
  }, [threadItems.length])

  useEffect(() => {
    if (!focusTurnAnchor) {
      focusedTurnAnchorRef.current = null
      return
    }
    const focusKey = `${conversationId}\0${focusTurnAnchor}`
    if (focusedTurnAnchorRef.current === focusKey) return
    const index = threadItems.findIndex(
      (item) => item.kind === "turn" && item.group.id === focusTurnAnchor
    )
    const scrollApi = scrollApiRef.current
    if (index < 0 || !scrollApi) return
    scrollApi.scrollToIndex(index, { align: "start", smooth: true })
    focusedTurnAnchorRef.current = focusKey
  }, [conversationId, focusTurnAnchor, threadItems])

  const hasPersistedHistoryRows = threadItems.some(
    (item) => item.kind === "turn" && item.phase === "persisted"
  )

  const hasRenderableContent =
    threadItems.length > 0 ||
    Boolean(liveMessage) ||
    (useIncrementalLive && (hasLiveTranscript || showLiveFooter))

  if (detailLoading && !hasRenderableContent) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="flex items-center gap-2 text-sm text-muted-foreground">
          <Loader2 className="h-4 w-4 animate-spin" />
          <span>{t("loading")}</span>
        </div>
      </div>
    )
  }

  // An ACP load failure replaces content only when there is nothing to show
  // (e.g. the DB detail also failed). When the local DB has the conversation,
  // keep the transcript visible — the failure is not silent: the detail panel
  // renders the load error as a banner in the composer area (with Reload /
  // New session actions), so the user still learns that a follow-up message
  // can't extend this thread.
  const blockingLoadError = hasRenderableContent ? null : (acpLoadError ?? null)
  const fallbackLoadError =
    detailError && !hasRenderableContent ? detailError : null
  const renderedLoadError = blockingLoadError ?? fallbackLoadError
  if (renderedLoadError) {
    const showReload = Boolean(
      onReload && canReloadSessionLoadError(acpLoadErrorCode)
    )
    const showActions = showReload || Boolean(onNewSession)
    const reloading = detailLoading
    return (
      <div role="alert" className="flex h-full items-center justify-center p-6">
        <div className="flex max-w-md flex-col items-center gap-4 text-center">
          <AlertCircle
            aria-hidden="true"
            className="h-8 w-8 text-destructive"
          />
          <div className="space-y-1">
            <h3 className="text-sm font-medium">{t("errorTitle")}</h3>
            <p className="text-sm text-muted-foreground break-words">
              {renderedLoadError}
            </p>
          </div>
          {showActions && (
            <div className="flex flex-wrap items-center justify-center gap-2">
              {showReload && onReload && (
                <Button
                  size="sm"
                  onClick={onReload}
                  disabled={reloading}
                  aria-busy={reloading}
                >
                  {reloading ? (
                    <Loader2
                      aria-hidden="true"
                      className="me-1.5 h-4 w-4 animate-spin"
                    />
                  ) : (
                    <RefreshCw aria-hidden="true" className="me-1.5 h-4 w-4" />
                  )}
                  {t("errorActionReload")}
                </Button>
              )}
              {onNewSession && (
                <Button size="sm" variant="outline" onClick={onNewSession}>
                  <Plus aria-hidden="true" className="me-1.5 h-4 w-4" />
                  {t("errorActionNewSession")}
                </Button>
              )}
            </div>
          )}
        </div>
      </div>
    )
  }

  return (
    <div className="relative flex h-full min-h-0 flex-col">
      <MessageThread
        className="flex-1 min-h-0"
        resize={
          hasLiveTranscript || initialHistoryScrollActive ? "instant" : "smooth"
        }
      >
        <InitialHistoryScrollController
          pending={initialHistoryScrollActive}
          historyReady={historyLoadComplete}
          hasHistoryRows={hasPersistedHistoryRows}
          onFinish={finishInitialHistoryScroll}
        />
        <AutoScrollOnSend signal={sendSignal} />
        <VirtualizedMessageThread
          items={threadItems}
          getItemKey={getThreadItemKey}
          renderItem={renderThreadItem}
          emptyState={emptyState}
          header={
            historyWindow?.has_more_before ? (
              <div className="flex justify-center pb-2">
                <Button
                  type="button"
                  size="sm"
                  variant="outline"
                  onClick={onLoadOlderHistory}
                  disabled={detailHistoryLoadingOlder}
                  aria-busy={detailHistoryLoadingOlder}
                  data-testid="load-older-history"
                >
                  {detailHistoryLoadingOlder ? (
                    <Loader2
                      aria-hidden="true"
                      className="me-1.5 h-3.5 w-3.5 animate-spin"
                    />
                  ) : null}
                  {detailHistoryLoadingOlder
                    ? t("loading")
                    : t("loadOlderHistory")}
                </Button>
              </div>
            ) : null
          }
          footer={liveFooter}
          scrollApiRef={scrollApiRef}
          hasOlder={hasOlderTurns}
          isLoadingOlder={loadingOlderTurns}
          onLoadOlder={handleLoadOlder}
          loadOlderLabel={t("loadEarlier")}
          loadingOlderLabel={t("loadingEarlier")}
          prependEpoch={olderTurnsPrependEpoch}
          prependScopeKey={conversationId}
        />
        <MessageThreadScrollButton
          onBeforeScrollToBottom={() => {
            scrollApiRef.current?.footerScroll?.markAtBottom()
          }}
        />
      </MessageThread>
      <LiveTurnStatsBanner
        conversationId={conversationId}
        agentType={agentType}
        isStreaming={
          connStatus === "prompting" &&
          (useIncrementalLive ? hasLiveTranscript : Boolean(liveMessage))
        }
        isWaitingForSubagents={isWaitingForSubagents}
        waitingArmedAtMs={
          isWaitingForSubagents ? waitingForSubagentsArmedAtMs : null
        }
      />
      {/* Shared overlay stack pinned to the inline-start edge (top-left in LTR,
          top-right in RTL). A flex column keeps the order stable regardless of
          each panel's expand/collapse height: the message navigator first, then
          the plan panel, then the sub-agent panel. Empty panels render null and
          collapse out. Positioning lives here (not in the child overlays); the
          chips are "bullets" — flat on the start side (flush to the pinned
          edge), rounded on the end side — that expand toward the inline-end on
          hover. Logical `start-0` + `items-start` keep the anchor and the bullet
          on the same side, so the whole stack mirrors cleanly in RTL. */}
      <div className="pointer-events-none absolute start-0 top-4 z-20 flex max-w-[min(28rem,calc(100%-2rem))] flex-col items-start gap-2">
        {showMessageNav && userMessageCount > 0 && (
          <ConversationMessageNav
            count={userMessageCount}
            expanded={navExpanded}
            onToggle={setNavExpanded}
            entries={navEntries}
            scrollApiRef={scrollApiRef}
          />
        )}
        {useIncrementalLive ? (
          <LiveAgentPlanOverlay
            conversationId={conversationId}
            entries={historicalPlanEntries}
            planKey={historicalPlanKey}
            isStreaming={connStatus === "prompting"}
          />
        ) : (
          <AgentPlanOverlay
            key={agentPlanOverlayKey}
            message={liveMessage ?? null}
            entries={historicalPlanEntries}
            planKey={historicalPlanKey}
            defaultExpanded={false}
            isStreaming={connStatus === "prompting"}
          />
        )}
        {useIncrementalLive ? (
          <LiveAwareSubAgentOverlay
            conversationId={conversationId}
            durableConversationId={durableConversationId}
            agentType={agentType}
            isStreaming={connStatus === "prompting"}
            historicalDelegations={allSessionDelegations}
            historicalActivities={sessionActivities}
            historicalKey={subAgentOverlayKey}
            workspaceRootPath={workspaceRootPath}
            isActive={isActive}
            onResumeRoot={onResumeRoot}
            onOpenRootConversation={onOpenRootConversation}
          />
        ) : (
          <SubAgentOverlay
            key={subAgentOverlayKey}
            delegations={allSessionDelegations}
            activities={sessionActivities}
            overlayKey={subAgentOverlayKey}
            defaultExpanded
            conversationId={durableConversationId}
            workflowGraph={workflowGraph}
            workspaceRootPath={workspaceRootPath}
            isActive={isActive}
            onResumeRoot={onResumeRoot}
            onOpenRootConversation={onOpenRootConversation}
          />
        )}
      </div>
    </div>
  )
}
