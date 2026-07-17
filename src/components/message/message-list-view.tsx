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
  selectHistoricalTimelineTurns,
  selectTimelineTurns,
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
import { ContentPartsRenderer } from "./content-parts-renderer"
import { LiveTranscriptRow } from "./live-transcript-row"
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
import { useSessionStats } from "@/contexts/session-stats-context"
import { AgentPlanOverlay } from "@/components/chat/agent-plan-overlay"
import { SubAgentOverlay } from "@/components/chat/sub-agent-overlay"
import {
  inferLiveToolName,
  normalizeToolName,
} from "@/lib/tool-call-normalization"
import { isDelegateToAgentToolName } from "@/lib/delegation-card"
import type { DelegationCardSource } from "@/hooks/use-delegation-card-model"
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
  CopyIcon,
  Info,
  Loader2,
  Plus,
  RefreshCw,
} from "lucide-react"
import { Button } from "@/components/ui/button"
import { useTranslations } from "next-intl"
import {
  buildPlanKey,
  extractLatestPlanEntriesFromMessages,
} from "@/lib/agent-plan"
import type {
  AgentType,
  ConnectionStatus,
  MessageTurn,
  SessionStats,
} from "@/lib/types"
import { copyTextToClipboard } from "@/lib/utils"
import { VirtualizedMessageThread } from "@/components/message/virtualized-message-thread"
import {
  ConversationMessageNav,
  type MessageNavEntry,
} from "@/components/message/conversation-message-nav"
import type { MessageScrollContextValue } from "@/components/message/message-scroll-context"
import { extractSessionFilesGrouped } from "@/lib/session-files"
import { unescapeComposerText } from "@/lib/composer-copy-text"
import { useStickToBottomContext } from "use-stick-to-bottom"

interface MessageListViewProps {
  conversationId: number
  agentType: AgentType
  connStatus?: ConnectionStatus | null
  isActive?: boolean
  sendSignal?: number
  sessionStats?: SessionStats | null
  detailLoading?: boolean
  detailError?: string | null
  /**
   * Set when the agent rejected `session/load` non-recoverably (e.g. the
   * historical session_id was deleted). Takes precedence over `detailError`
   * AND the renderable-content gate: even when the local DB has the full
   * message history, the user must explicitly choose Reload or start a new
   * conversation since the agent can't continue this thread.
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
}

export function canReloadSessionLoadError(
  code: string | null | undefined
): boolean {
  return code !== "legacy_cli_session"
}

type AutolinkableTextPart = Extract<AdaptedContentPart, { type: "text" }>

const EMPTY_AUTOLINKABLE_TEXT_PARTS: ReadonlySet<AutolinkableTextPart> =
  new Set()

interface ResolvedMessageGroup {
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
  model?: string | null
  models?: string[]
  /**
   * Wall-clock completion time supplied by the Rust parser. For merged
   * sub-turns this reflects the last sub-turn's completion (inherited
   * automatically via `{ ...last.group }`), not first-start + accumulated
   * duration.
   */
  completed_at?: string | null
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

type ThreadRenderItem =
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
  parts: AdaptedContentPart[],
  out: DelegationCardSource[]
): void {
  for (const part of parts) {
    if (part.type === "tool-call") {
      if (
        part.toolCallId &&
        isDelegateToAgentToolName(normalizeToolName(part.toolName))
      ) {
        out.push({
          parentToolUseId: part.toolCallId,
          input: part.input ?? null,
          output: part.output ?? null,
          errorText: part.errorText ?? null,
          state: part.state,
          meta: part.meta ?? null,
        })
      }
    } else if (part.type === "tool-group") {
      collectDelegationSources(part.items, out)
    } else if (part.type === "goal-run") {
      collectDelegationSources(part.items, out)
    }
  }
}

const CollapsibleSystemMessage = memo(function CollapsibleSystemMessage({
  group,
}: {
  group: ResolvedMessageGroup
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
            <ContentPartsRenderer parts={group.parts} role={group.role} />
          </div>
        </div>
      )}
    </div>
  )
})

function extractTextFromParts(parts: AdaptedContentPart[]): string {
  return parts
    .flatMap((p): string[] => {
      if (p.type === "text") return [p.text]
      if (p.type === "goal-run") return [extractTextFromParts(p.items)]
      return []
    })
    .filter((text) => text.length > 0)
    .join("\n")
}

type AssistantTurnItem = Extract<ThreadRenderItem, { kind: "turn" }>

function isEmptyTurnItem(item: ThreadRenderItem): boolean {
  if (item.kind !== "turn") return false
  const g = item.group
  if (g.parts.length > 0) return false
  if (g.resources.length > 0) return false
  if (g.images.length > 0) return false
  return true
}

/**
 * Collapse runs of consecutive assistant turn render items into a single
 * synthetic turn so tool-groups straddling a turn boundary fold into one
 * collapsible. Empty (no-content) turn items are treated as transparent and
 * do not break the run — that handles cases where parsers leave empty
 * placeholder turns between tool exchanges.
 */
function mergeConsecutiveAssistantTurns(
  items: ThreadRenderItem[]
): ThreadRenderItem[] {
  const result: ThreadRenderItem[] = []
  const skipped: ThreadRenderItem[] = []
  let buffer: AssistantTurnItem[] = []

  const flush = () => {
    if (buffer.length === 0) {
      // Drain any skipped (empty) items collected since last flush
      for (const s of skipped) result.push(s)
      skipped.length = 0
      return
    }

    if (buffer.length === 1) {
      result.push(buffer[0])
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
      const seenModels = new Set<string>()
      const mergedModels: string[] = []
      for (const it of buffer) {
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
        if (it.group.model && !seenModels.has(it.group.model)) {
          seenModels.add(it.group.model)
          mergedModels.push(it.group.model)
        }
      }

      result.push({
        ...last,
        key: `merged-${first.key}`,
        // Concatenate every sub-turn's raw turns so the artifacts card sees all
        // file edits across the merged reply, not just the last sub-turn.
        sourceTurns: buffer.flatMap((b) => b.sourceTurns),
        group: {
          ...last.group,
          id: first.group.id,
          parts: mergedParts,
          autolinkableTextParts:
            mergedAutolinkableTextParts.size > 0
              ? mergedAutolinkableTextParts
              : EMPTY_AUTOLINKABLE_TEXT_PARTS,
          usage: mergedUsage,
          duration_ms: mergedDuration,
          model: mergedModels[0] ?? last.group.model,
          models: mergedModels.length > 1 ? mergedModels : undefined,
        },
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

const HistoricalMessageGroup = memo(function HistoricalMessageGroup({
  group,
  dimmed = false,
  showStats = true,
  previousUserIndex = null,
  isResponseComplete = true,
  sourceTurns,
  renderKind = "historicalRow",
}: {
  group: ResolvedMessageGroup
  dimmed?: boolean
  showStats?: boolean
  previousUserIndex?: number | null
  isResponseComplete?: boolean
  sourceTurns?: MessageTurn[]
  renderKind?: "historicalRow" | "liveRow"
}) {
  streamingPerfRecorder.countRender(renderKind)
  if (group.role === "system") {
    return <CollapsibleSystemMessage group={group} />
  }

  return (
    <div className={dimmed ? "opacity-70" : undefined}>
      <Message from={group.role}>
        {group.role === "user" && group.images.length > 0 ? (
          <UserImageAttachments images={group.images} className="self-end" />
        ) : null}
        {group.role === "user" ? (
          <div className="group/user-msg flex w-fit ml-auto max-w-full items-start gap-1">
            <UserMessageCopyButton parts={group.parts} />
            <MessageContent>
              <ContentPartsRenderer parts={group.parts} role={group.role} />
            </MessageContent>
          </div>
        ) : (
          <MessageContent>
            <ContentPartsRenderer
              parts={group.parts}
              role={group.role}
              autolinkLocalPathParts={
                isResponseComplete ? group.autolinkableTextParts : undefined
              }
            />
          </MessageContent>
        )}
        {group.role === "user" && group.resources.length > 0 ? (
          <UserResourceLinks resources={group.resources} className="self-end" />
        ) : null}
      </Message>
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
          model={group.model}
          models={group.models}
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
  message: LiveMessage
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
      })
    )
    if (!isDelegateToAgentToolName(toolName)) continue
    const resolvedOutput =
      block.info.raw_output_chunks.length > 0
        ? block.info.raw_output_chunks.join("")
        : block.info.content
    liveSources.push({
      parentToolUseId: block.info.tool_call_id,
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

/** Narrow-subscription live stats — parent list does not re-render per token. */
const LiveTurnStatsFromTranscript = memo(function LiveTurnStatsFromTranscript({
  conversationId,
  agentType,
}: {
  conversationId: number
  agentType: AgentType
}) {
  const snap = useLiveTranscriptConversation(conversationId)
  const message = useMemo(
    () => (snap ? liveSnapshotToLiveMessage(snap) : null),
    [snap]
  )
  if (!message) return null
  return <LiveTurnStats message={message} agentType={agentType} isStreaming />
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
 * Merge live-transcript delegation rows into the full historical list.
 * Live rows win on `parentToolUseId` (fresher status/output while streaming);
 * live-only ids (not yet adapted into history) are appended in order.
 */
function mergeLiveAndHistoricalDelegations(
  historical: DelegationCardSource[],
  live: DelegationCardSource[]
): DelegationCardSource[] {
  if (live.length === 0) return historical
  if (historical.length === 0) return live

  const liveById = new Map(
    live.map((source) => [source.parentToolUseId, source])
  )
  const seen = new Set<string>()
  const merged: DelegationCardSource[] = []

  for (const source of historical) {
    const liveSource = liveById.get(source.parentToolUseId)
    if (liveSource) {
      merged.push(liveSource)
      seen.add(source.parentToolUseId)
    } else {
      merged.push(source)
      seen.add(source.parentToolUseId)
    }
  }
  for (const source of live) {
    if (!seen.has(source.parentToolUseId)) {
      merged.push(source)
    }
  }
  return merged
}

/**
 * Sub-agent overlay: full conversation history, with live-transcript rows
 * preferred for the in-flight turn so status updates without waiting on
 * historical adaptation.
 */
const LiveAwareSubAgentOverlay = memo(function LiveAwareSubAgentOverlay({
  conversationId,
  isStreaming,
  historicalDelegations,
  historicalKey,
}: {
  conversationId: number
  isStreaming: boolean
  historicalDelegations: DelegationCardSource[]
  historicalKey: string
}) {
  const snap = useLiveTranscriptConversation(
    isStreaming ? conversationId : null
  )
  const liveDelegations = useMemo(() => {
    if (!snap || !isStreaming) return EMPTY_DELEGATIONS
    return extractLiveDelegationSources(liveSnapshotToLiveMessage(snap))
  }, [snap, isStreaming])
  const delegations = useMemo(
    () =>
      mergeLiveAndHistoricalDelegations(historicalDelegations, liveDelegations),
    [historicalDelegations, liveDelegations]
  )
  // Conversation-scoped key so expand/collapse survives new turns and the
  // live↔historical handoff (unlike a per-message key which remounts the panel).
  return (
    <SubAgentOverlay
      key={historicalKey}
      delegations={delegations}
      overlayKey={historicalKey}
      defaultExpanded
    />
  )
})

export function MessageListView({
  conversationId,
  agentType,
  connStatus,
  isActive = true,
  sendSignal = 0,
  sessionStats = null,
  detailLoading = false,
  detailError = null,
  acpLoadError = null,
  acpLoadErrorCode = null,
  hideEmptyState = false,
  onReload,
  onNewSession,
  showMessageNav = true,
}: MessageListViewProps) {
  const t = useTranslations("Folder.chat.messageList")
  const sharedT = useTranslations("Folder.chat.shared")
  const useIncrementalLive = useStreamingPerformanceFlag(
    "incremental_live_transcript"
  )

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

  const { setSessionStats } = useSessionStats()

  streamingPerfRecorder.countRender("historicalThread")

  // After React commit, drain pending deliveries and let the recorder schedule
  // a coalesced next-paint RAF. Paint scheduling lives on the recorder so
  // rapid re-render effect cleanup cannot cancel samples before paint.
  useLayoutEffect(() => {
    streamingPerfRecorder.markReactCommit()
  })

  useEffect(() => {
    if (isActive) {
      setSessionStats(sessionStats)
    }
  }, [isActive, sessionStats, setSessionStats])

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

  const { threadItems, nonStreamingAdapted } = useMemo(() => {
    const allTurns = timelineTurns.map((item) => item.turn)
    const streamingIndices = new Set<number>()
    const inProgressToolCallIdsByIndex = new Map<number, Set<string>>()
    timelineTurns.forEach((item, i) => {
      if (item.phase === "streaming") {
        streamingIndices.add(i)
        if (item.inProgressToolCallIds && item.inProgressToolCallIds.size > 0) {
          inProgressToolCallIdsByIndex.set(i, item.inProgressToolCallIds)
        }
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

    // Map each adapted message directly to a render item (1:1).
    // Backend group_into_turns() already ensures each turn is a complete unit.
    const rawItems: ThreadRenderItem[] = allAdapted.map((msg, i) => {
      const phase = timelineTurns[i].phase
      const role = msg.role === "tool" ? "assistant" : msg.role
      let group = groupCache.get(msg)
      if (!group) {
        group = {
          id: msg.id,
          role,
          parts: msg.content,
          resources: msg.userResources ?? [],
          images: msg.userImages ?? [],
          autolinkableTextParts: topLevelAssistantTextParts(msg),
          usage: msg.usage,
          duration_ms: msg.duration_ms,
          model: msg.model,
          completed_at: msg.completed_at,
        }
        groupCache.set(msg, group)
      }
      return {
        // Include phase so a turn that briefly coexists across phases (e.g.
        // a streaming turn that has just been promoted to localTurns while the
        // liveMessage is still attached) doesn't collide with itself in the
        // virtualized list. Index disambiguates further within a phase.
        key: `${phase}-${msg.id}-${i}`,
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
    const items = mergeConsecutiveAssistantTurns(rawItems)

    // Compute showStats, isRoleTransition, and previousUserIndex for each turn.
    // previousUserIndex points at the closest preceding user turn (used by the
    // post-stream stats row's "jump to previous user message" button).
    let lastUserIdx: number | null = null
    for (let idx = 0; idx < items.length; idx++) {
      const item = items[idx]
      if (item.kind !== "turn") continue

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

    return { threadItems: items, nonStreamingAdapted: nonStreaming }
  }, [
    adapterText,
    connStatus,
    sessionSyncState,
    timelineTurns,
    turnAdapter,
    groupCache,
    useIncrementalLive,
  ])

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
      />
    )
  }, [showLiveFooter, conversationId, agentType])

  const historicalPlanEntries = useMemo(
    () => extractLatestPlanEntriesFromMessages(nonStreamingAdapted),
    [nonStreamingAdapted]
  )
  const historicalPlanKey = useMemo(
    () => buildPlanKey(historicalPlanEntries),
    [historicalPlanEntries]
  )

  const renderThreadItem = useCallback((item: ThreadRenderItem) => {
    switch (item.kind) {
      case "turn": {
        const pt = item.isRoleTransition ? 16 : 0
        return (
          <div style={pt > 0 ? { paddingTop: pt } : undefined}>
            <HistoricalMessageGroup
              group={item.group}
              dimmed={item.phase === "optimistic"}
              showStats={item.showStats}
              previousUserIndex={item.previousUserIndex}
              isResponseComplete={item.phase === "persisted"}
              sourceTurns={item.sourceTurns}
              renderKind={
                item.phase === "streaming" ? "liveRow" : "historicalRow"
              }
            />
          </div>
        )
      }
      case "typing":
        return <PendingTypingIndicator />
      default:
        return null
    }
  }, [])

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

  // All sub-agents delegated across the conversation (every assistant turn).
  // Timeline order is preserved so older delegations stay above newer ones.
  // The live streaming path merges fresher transcript rows on top of this list
  // (see `LiveAwareSubAgentOverlay`); a non-delegating later reply no longer
  // clears earlier history.
  const allSessionDelegations = useMemo(() => {
    const out: DelegationCardSource[] = []
    for (const item of threadItems) {
      if (item.kind === "turn" && item.group.role === "assistant") {
        collectDelegationSources(item.group.parts, out)
      }
    }
    return out.length > 0 ? out : EMPTY_DELEGATIONS
  }, [threadItems])
  // Conversation-scoped so expand/collapse is retained across turns.
  const subAgentOverlayKey = `subagents-${conversationId}`

  // --- Message navigator panel ------------------------------------------------
  // Lifted scroll handle so the panel (which lives in the overlay stack, outside
  // the MessageScrollProvider subtree) can drive scrollToIndex.
  const scrollApiRef = useRef<MessageScrollContextValue | null>(null)
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

  // ACP load failures always replace content: even when the local DB has
  // the conversation, the agent can't resume it, so silently rendering
  // the history would mislead the user into thinking a follow-up message
  // would extend the same thread.
  const blockingLoadError = acpLoadError ?? null
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
        resize={hasLiveTranscript ? "instant" : "smooth"}
      >
        <AutoScrollOnSend signal={sendSignal} />
        <VirtualizedMessageThread
          items={threadItems}
          getItemKey={getThreadItemKey}
          renderItem={renderThreadItem}
          emptyState={emptyState}
          footer={liveFooter}
          scrollApiRef={scrollApiRef}
        />
        <MessageThreadScrollButton
          onBeforeScrollToBottom={() => {
            scrollApiRef.current?.footerScroll?.markAtBottom()
          }}
        />
      </MessageThread>
      {useIncrementalLive
        ? hasLiveTranscript &&
          connStatus === "prompting" && (
            <LiveTurnStatsFromTranscript
              conversationId={conversationId}
              agentType={agentType}
            />
          )
        : liveMessage &&
          connStatus === "prompting" && (
            <LiveTurnStats
              message={liveMessage}
              agentType={agentType}
              isStreaming={connStatus === "prompting"}
            />
          )}
      {/* Shared overlay stack pinned to the inline-start edge (top-left in LTR,
          top-right in RTL). A flex column keeps the order stable regardless of
          each panel's expand/collapse height: the message navigator first, then
          the plan panel, then the sub-agent panel. Empty panels render null and
          collapse out. Positioning lives here (not in the child overlays); the
          chips are "bullets" — flat on the start side (flush to the pinned
          edge), rounded on the end side — that expand toward the inline-end on
          hover. Logical `start-0` + `items-start` keep the anchor and the bullet
          on the same side, so the whole stack mirrors cleanly in RTL. */}
      <div className="pointer-events-none absolute start-0 top-4 z-20 flex max-w-[min(22rem,calc(100%-2rem))] flex-col items-start gap-2">
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
            isStreaming={connStatus === "prompting"}
            historicalDelegations={allSessionDelegations}
            historicalKey={subAgentOverlayKey}
          />
        ) : (
          <SubAgentOverlay
            key={subAgentOverlayKey}
            delegations={allSessionDelegations}
            overlayKey={subAgentOverlayKey}
            defaultExpanded
          />
        )}
      </div>
    </div>
  )
}
