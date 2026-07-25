import { create } from "zustand"
import type {
  LiveMessage,
  ToolCallInfo,
} from "@/contexts/acp-connections-context"
import { getFolderConversation } from "@/lib/api"
import {
  cacheCompletedStreamingPartition,
  clearCompletedStreamingPartitions,
  completeStreamingMarkdown,
  joinStreamingMarkdown,
} from "@/lib/markdown/incremental-stream-blocks"
import { streamingPerfRecorder } from "@/lib/perf/streaming-perf-recorder"
import { registerBackendScopedStoreReset } from "@/stores/backend-scoped-store-reset"
import { liveTranscriptStore } from "@/stores/live-transcript-store"
import type {
  AgentExecutionStats,
  AgentType,
  DbConversationDetail,
  DelegationActivityView,
  MessageTurn,
  PlanEntryInfo,
  SessionStats,
  ToolCallStatus,
  TurnOutcome,
  TurnUsage,
} from "@/lib/types"
import {
  inferLiveToolName,
  parseGoalUpdateTitle,
} from "@/lib/tool-call-normalization"
import { COLLAB_AGENT_TOOL_NAME, mergeCollabOp } from "@/lib/collab-tool"
import { collapseLiveCollabBlocks } from "@/lib/collab-collapse"
import { kimiTodoWriteEntries } from "@/lib/plan-parse"
import { toErrorMessage } from "@/lib/app-error"
import {
  deriveNativeActivitiesFromToolCalls,
  type ToolFieldForActivity,
} from "@/lib/delegation-activity"
import { BACKGROUND_TASK_MARKER } from "@/lib/background-agent"

/**
 * Conversation-runtime shared state as a Zustand store — the per-conversation
 * session map (DB detail, optimistic/local/streaming turns, live message, sync
 * state) plus the reducer that drives it.
 *
 * Migrated from a React `useReducer` context whose single merged value flipped
 * on every ~16ms streaming token, re-rendering every consumer. The reducer and
 * all pure timeline-building helpers below are preserved verbatim; only the
 * React wiring (context value, per-render callbacks) is replaced. Components
 * subscribe to the narrowest slice — their own session
 * (`byConversationId.get(id)`) or derived timeline (`selectTimelineTurns`) — so
 * an unrelated conversation's stream no longer re-renders them; callbacks read
 * fresh state via `useConversationRuntimeStore.getState()`.
 *
 * The thin `ConversationRuntimeProvider` / `useConversationRuntime()` shim in
 * `contexts/conversation-runtime-context.tsx` re-exports this module for
 * backwards compatibility.
 */
export type ConversationSyncState = "idle" | "awaiting_persist"

export type ConversationTimelinePhase = "persisted" | "optimistic" | "streaming"

/**
 * Abort-fence identity for a user-stop cancel reconciliation coordinator.
 * `completionSeq` is the accepted `turn_complete` EventEnvelope.seq.
 * `cancelGeneration` is a dedicated per-session counter (not `fetchGeneration`).
 */
export interface CancelCompletionKey {
  conversationId: number
  connectionId: string
  completionSeq: number
  activeTurnToken: string | null
  providerTurnId: string
  cancelGeneration: number
}

/** Sequential delays (ms) before each raw detail attempt — non-overlapping. */
export const CANCEL_RECONCILE_DELAYS_MS = [100, 300, 1000] as const

/** Stable empty list for Zustand selectors when no activities exist. */
export const EMPTY_DELEGATION_ACTIVITIES: DelegationActivityView[] = []

export interface ConversationTimelineTurn {
  key: string
  turn: MessageTurn
  phase: ConversationTimelinePhase
  // Tool call IDs whose results are still streaming (only set for streaming-phase items).
  // The adapter uses this to keep the tool in "running" state while exposing partial output.
  inProgressToolCallIds?: Set<string>
}

/**
 * One out-of-turn overlay turn pushed by the backend transcript watcher via a
 * `background_activity` event, tagged with the transcript byte offset it was
 * parsed through. `FETCH_DETAIL_SUCCESS` retires entries whose watermark the
 * refetched detail's `transcript_watermark` has caught up to (`>=`) — the
 * race-free hand-off from overlay to persisted turns (both sides measure the
 * SAME file, so "caught up" means the detail literally contains those bytes).
 */
export interface BackgroundOverlayEntry {
  turn: MessageTurn
  watermark: number
}

/**
 * A settled async sub-agent whose launch card couldn't be flipped yet because
 * its launching turn hasn't been promoted into `localTurns` (the dominant case:
 * with #870 holding the turn open, the task settles seconds BEFORE the turn
 * completes, so at settle time the launch tool call is still in `liveMessage`,
 * un-patchable). Queued by `RESOLVE_BACKGROUND_TASK`, drained by `COMPLETE_TURN`
 * once promotion surfaces the tool_result block. Matched by `toolUseId`.
 */
export interface PendingBackgroundSettlement {
  toolUseId: string
  taskId: string
  status: string
  summary: string | null
  result: string | null
}

/**
 * Backstop bound on the overlay when retirement can't run (a refetch that
 * keeps failing — server unreachable — while cron//loop turns keep arriving).
 * Oldest entries are dropped first: they are already-persisted facts, so the
 * next SUCCESSFUL detail fetch shows them again; only the degraded window
 * loses scrollback. The normal bound is much tighter — the connections layer
 * triggers a self-healing refetch well below this (see the
 * `background_activity` handler).
 */
export const BACKGROUND_OVERLAY_HARD_CAP = 300

export interface ConversationRuntimeSession {
  conversationId: number
  externalId: string | null

  // The DB row id behind this runtime session, when it differs from the
  // session key. A conversation started as a new-chat draft keeps its
  // virtual (negative) `conversationId` as the runtime key for the tab's
  // whole life, while the real row is created on first send — any fetch on
  // behalf of this session must use THIS id (`refetchDetail` resolves it),
  // or the backend is asked for a conversation that doesn't exist and the
  // stale live buffers silently never get replaced. Null until bound; for
  // sessions keyed by a real DB id the key itself is the fallback.
  dbConversationId: number | null

  // DB data (cold open only)
  detail: DbConversationDetail | null
  detailLoading: boolean
  detailError: string | null

  // ACP `session/load` failed in a non-recoverable way (currently only when
  // the agent reports ResourceNotFound for the historical session_id). Set
  // by the connections layer via setAcpLoadError; cleared by the user
  // pressing Reload, by a successful detail refetch, or when a new ACP
  // session takes over.
  acpLoadError: string | null

  // Active session accumulated turns (promoted optimistic + completed streaming)
  localTurns: MessageTurn[]

  // Out-of-turn transcript overlay: async task-notification completions, the
  // agent's continued work after them, cron//loop autonomous turns — parsed
  // from the agent's own session file by the backend watcher and upserted here
  // by `background_activity` events (keyed by turn id; a still-growing turn is
  // replaced in place). These are already-persisted facts shown ahead of the
  // next detail refetch; the watermark rule above retires them once a refetch
  // catches up, so overlay and persisted copies never coexist in the timeline.
  backgroundTurns: BackgroundOverlayEntry[]

  // Settled async sub-agents awaiting their launch card's in-memory flip until
  // the launching turn promotes into `localTurns` (see
  // `PendingBackgroundSettlement`). Drained by `COMPLETE_TURN`.
  pendingBackgroundSettlements: PendingBackgroundSettlement[]

  // Temporary state
  optimisticTurns: MessageTurn[]
  liveMessage: LiveMessage | null

  // Sync
  syncState: ConversationSyncState
  activeTurnToken: string | null

  // True when THIS client DROVE the most recently promoted turn (an owner send,
  // `awaiting_persist`), false when it merely VIEWED that turn. An owner's just-
  // promoted reply lives only in `localTurns` and may not be flushed to the
  // transcript yet (an ~8ms write race — see `completeTurn`'s no-refetch note),
  // so a viewer-sync refetch must never clobber it; a VIEWED turn's reply is
  // already persisted (it completed on the owner before this client saw the
  // edge) and is safe to fold from disk. `completeTurn` collapses both owner and
  // viewer to `idle`, erasing the live distinction, so it is captured here at
  // promotion time. Consumed by `isPureViewerSession`.
  lastTurnOwned: boolean

  // Read-only delegation-child viewer marker. When true, `getTimelineTurns`
  // suppresses the persisted copy of the (single) reply turn while this
  // session has a live or just-promoted reply — so the sub-agent dialog shows
  // the kickoff + live/local reply exactly once, never a persisted partial
  // beside the live stream. Off for normal panels (which never set it), so
  // their multi-turn history is untouched. See `getTimelineTurns`.
  liveOwnsActiveTurn: boolean

  // Known kickoff prompt text for a delegation-child viewer (the parent's
  // `delegate_to_agent` task, available synchronously in the card). While
  // `liveOwnsActiveTurn` is set and the persisted transcript has not yet
  // surfaced the child's user turn (the agent CLI writes its JSONL
  // asynchronously, so the DB read lags the stream by up to seconds),
  // `getTimelineTurns` synthesizes the kickoff user turn from this so it
  // shows immediately above the streaming reply instead of after the child
  // finishes. Cleared automatically once the real persisted user turn lands.
  delegationKickoffText: string | null

  // Session-level stats (token usage, context window, etc.)
  sessionStats: SessionStats | null

  /**
   * Read-only native/Codeg activity projection for the latest assistant
   * materialization (live stream and COMPLETE_TURN). Never replaces tool
   * blocks; consumed by the sub-agent overlay when present.
   */
  delegationActivities: DelegationActivityView[]
  // Number of persisted assistant turns that predate this session's `localTurns`
  // — captured at send time (first optimistic turn of a batch), when `detail`
  // is settled history. The post-turn reparse (`syncTurnMetadata`) slices this
  // many turns off the front of the fresh parse before aligning the rest to
  // `localTurns`, so it never folds a historical (or later-refetched partial)
  // turn's stats into a new reply. `null` when no user-initiated batch is in
  // flight (e.g. the sub-agent adopt path, which has no optimistic send); the
  // reparse then treats the whole parse as this session's, matching the
  // pre-capture behavior. See `computeTurnMetadataPatches`.
  historyAssistantBaseline: number | null

  // Cleanup
  pendingCleanup: boolean

  /** Terminal persistence failed to replace the last visible delegate reply. */
  delegateSyncError: string | null

  /**
   * Pending user-stop cancel reconciliation fence. While non-null, automatic
   * destructive detail commits (`FETCH_DETAIL_SUCCESS` without preserveLive /
   * viewer + delegate terminal sync) are suppressed. Cleared on the lifecycle
   * table paths (success, exhaustion, Manual Reload, new prompt, remove, rebind).
   */
  pendingCancel: CancelCompletionKey | null
}

interface ConversationRuntimeState {
  byConversationId: Map<number, ConversationRuntimeSession>
  conversationIdByExternalId: Map<string, number>
}

const initialState: ConversationRuntimeState = {
  byConversationId: new Map(),
  conversationIdByExternalId: new Map(),
}

// Shared stable reference for the "no session" timeline result, so callers
// memoizing on the returned array (MessageListView's `threadItems`) don't see
// a fresh array on every render for conversations that don't exist yet.
const EMPTY_TIMELINE: ConversationTimelineTurn[] = []

/**
 * Cache key for the historical (non-streaming) timeline. Intentionally excludes
 * live message *content* so content-only `SET_LIVE_MESSAGE` appends reuse the
 * same historical array + entry references. Live identity (`id` / `startedAt`)
 * still invalidates once at turn start and once at handoff.
 */
interface HistoricalTimelineCacheKey {
  detail: DbConversationDetail | null
  localTurns: MessageTurn[]
  backgroundTurns: BackgroundOverlayEntry[]
  optimisticTurns: MessageTurn[]
  liveOwnsActiveTurn: boolean
  delegationKickoffText: string | null
  liveMessageId: string | null
  liveStartedAt: number | null
}

interface HistoricalTimelineCacheEntry {
  key: HistoricalTimelineCacheKey
  value: ConversationTimelineTurn[]
}

/** Per-conversation historical timeline cache (session-keyed via conversationId). */
const historicalTimelineCache = new Map<number, HistoricalTimelineCacheEntry>()

function sameHistoricalKey(
  left: HistoricalTimelineCacheKey,
  right: HistoricalTimelineCacheKey
): boolean {
  return (
    left.detail === right.detail &&
    left.localTurns === right.localTurns &&
    left.backgroundTurns === right.backgroundTurns &&
    left.optimisticTurns === right.optimisticTurns &&
    left.liveOwnsActiveTurn === right.liveOwnsActiveTurn &&
    left.delegationKickoffText === right.delegationKickoffText &&
    left.liveMessageId === right.liveMessageId &&
    left.liveStartedAt === right.liveStartedAt
  )
}

function buildHistoricalKey(
  session: ConversationRuntimeSession
): HistoricalTimelineCacheKey {
  return {
    detail: session.detail,
    localTurns: session.localTurns,
    backgroundTurns: session.backgroundTurns,
    optimisticTurns: session.optimisticTurns,
    liveOwnsActiveTurn: session.liveOwnsActiveTurn,
    delegationKickoffText: session.delegationKickoffText,
    liveMessageId: session.liveMessage?.id ?? null,
    liveStartedAt: session.liveMessage?.startedAt ?? null,
  }
}

function dedupeTimelineByRoleAwareId(
  result: ConversationTimelineTurn[]
): ConversationTimelineTurn[] {
  // Retain rule is role-aware (all entries sharing an id are the same
  // underlying turn, so the role is unambiguous):
  //   - ASSISTANT (and any non-user): keep the LAST occurrence.
  //   - USER: keep the FIRST occurrence.
  // The key includes the role, not just the id, so the merge only ever
  // collapses entries that are genuinely the same turn (same id AND role).
  // Runs for every timeline entry on every streaming token, so avoid
  // `JSON.stringify`. `role` is a fixed enum with no spaces, so the first space
  // unambiguously splits role from id — a collision-free, far cheaper key.
  const retainKey = (turn: MessageTurn) => `${turn.role} ${turn.id}`
  const retainIndexByKey = new Map<string, number>()
  result.forEach((entry, i) => {
    const key = retainKey(entry.turn)
    const existing = retainIndexByKey.get(key)
    // First sighting always records; later sightings overwrite only for
    // non-user turns (keep-last). User turns keep their first index.
    if (existing === undefined || entry.turn.role !== "user") {
      retainIndexByKey.set(key, i)
    }
  })
  return retainIndexByKey.size === result.length
    ? result
    : result.filter(
        (entry, i) => retainIndexByKey.get(retainKey(entry.turn)) === i
      )
}

type Action =
  | {
      type: "FETCH_DETAIL_START"
      conversationId: number
    }
  | {
      type: "FETCH_DETAIL_SUCCESS"
      conversationId: number
      detail: DbConversationDetail
      /**
       * Keep `liveMessage` / `optimisticTurns` / `localTurns` across this
       * detail load even though `syncState` isn't "awaiting_persist". The
       * sub-agent dialog sets this for a fetch issued while the child is
       * mid-stream: it loads the persisted detail to surface the user kickoff
       * turn, but the bridged/promoted reply must survive the fetch (otherwise
       * the streamed turn would blank until the next ContentDelta re-bridges
       * it, and a late-resolving partial could momentarily replace it).
       */
      preserveLive?: boolean
    }
  | {
      type: "SET_DELEGATE_SYNC_ERROR"
      conversationId: number
      error: string | null
    }
  | {
      type: "SET_LIVE_OWNS_ACTIVE_TURN"
      conversationId: number
      value: boolean
      /**
       * Optional kickoff prompt text to store alongside the flag. `undefined`
       * leaves the existing `delegationKickoffText` untouched (e.g. a pure
       * clear); a string (or null) overwrites it. The sub-agent dialog passes
       * the parent's known `delegate_to_agent` task so the kickoff user turn
       * can be synthesized before the async transcript catches up.
       */
      kickoffText?: string | null
    }
  | {
      type: "FETCH_DETAIL_ERROR"
      conversationId: number
      error: string
    }
  | {
      type: "COMPLETE_TURN"
      conversationId: number
      /**
       * Optional authoritative liveMessage from the caller. `session.liveMessage`
       * is kept current by the connection dispatch's liveMessage sink (writes
       * synchronously as each batch is applied — see `registerLiveMessageSink`),
       * so the fallback already holds the final chunk. The conversation panel
       * therefore omits it (it no longer subscribes to conn.liveMessage) and
       * relies on the sink-synced value; the sub-agent dialog's own child bridge
       * still passes it explicitly. When provided, it is preferred.
       */
      liveMessage?: LiveMessage | null
    }
  | {
      // Upsert out-of-turn overlay turns from a `background_activity` event
      // (see `BackgroundOverlayEntry`). Turns are keyed by id: new ids append
      // in event order, a re-emitted (still-growing) turn replaces its entry
      // in place, and every upserted entry adopts the event's watermark.
      type: "APPLY_BACKGROUND_ACTIVITY"
      conversationId: number
      turns: MessageTurn[]
      watermark: number
    }
  | {
      // An async sub-agent settled: flip its launch card in-memory by rewriting
      // the launching tool_result's `[[codeg-background-task]]` marker. If the
      // launching turn hasn't promoted into `localTurns` yet (settle precedes
      // turn completion under #870), queue it for `COMPLETE_TURN` to apply.
      type: "RESOLVE_BACKGROUND_TASK"
      conversationId: number
      settlement: PendingBackgroundSettlement
    }
  | {
      type: "APPEND_OPTIMISTIC_TURN"
      conversationId: number
      turn: MessageTurn
      turnToken: string
    }
  | {
      // Roll back an optimistic user turn that never reached the backend
      // (e.g. the send was rejected because a turn was already in flight, and
      // the draft is being re-queued instead). Resets syncState to idle when no
      // other optimistic turns remain so a stranded `awaiting_persist` doesn't
      // block the next detail reconciliation.
      type: "REMOVE_OPTIMISTIC_TURN"
      conversationId: number
      id: string
    }
  | {
      // Cross-client VIEWER synthesizes the sender's user turn from a
      // `user_message` event / snapshot. Idempotent + sender-guarded in the
      // reducer (never fires on a client that has its own in-flight send).
      type: "APPEND_VIEWER_USER_TURN"
      conversationId: number
      turn: MessageTurn
    }
  | {
      type: "SET_LIVE_MESSAGE"
      conversationId: number
      liveMessage: LiveMessage | null
      /**
       * When true, bypass the stale-reconnect-replay guard. The caller has
       * verified that the source connection is currently producing this
       * liveMessage (e.g. status === "prompting"), so the content is fresh
       * rather than a post-completion replay. Required for the rekey path
       * (close+reopen mid-turn): the runtime session for the persisted
       * conversation id is brand-new, has no liveMessage, and may already
       * see the user turn in `detail.turns` once cold-load resolves —
       * which would otherwise trigger the guard and drop the live
       * assistant content.
       */
      isLive?: boolean
    }
  | {
      type: "SET_EXTERNAL_ID"
      conversationId: number
      externalId: string | null
    }
  | {
      type: "SET_DB_CONVERSATION_ID"
      conversationId: number
      dbConversationId: number | null
    }
  | {
      type: "SET_SYNC_STATE"
      conversationId: number
      syncState: ConversationSyncState
    }
  | {
      type: "MIGRATE_CONVERSATION"
      fromConversationId: number
      toConversationId: number
    }
  | {
      type: "SET_PENDING_CLEANUP"
      conversationId: number
      pendingCleanup: boolean
    }
  | {
      type: "PATCH_TURN_METADATA"
      conversationId: number
      turnPatches: Array<{
        index: number
        usage?: TurnUsage | null
        duration_ms?: number | null
        model?: string | null
        completed_at?: string | null
        tool_meta?: Array<{
          tool_use_id: string
          meta: Record<string, unknown> | null
        }>
      }>
      sessionStats?: SessionStats | null
    }
  | {
      type: "SET_ACP_LOAD_ERROR"
      conversationId: number
      error: string | null
    }
  | {
      type: "RECORD_TURN_OUTCOME"
      conversationId: number
      connectionId: string
      completionSeq: number
      outcome: TurnOutcome
    }
  | {
      type: "START_CANCEL_RECONCILE"
      conversationId: number
      key: CancelCompletionKey
    }
  | {
      type: "RECONCILE_CANCELLED_TURN"
      conversationId: number
      detail: DbConversationDetail
      key: CancelCompletionKey
    }
  | {
      type: "CLEAR_CANCEL_RECONCILE"
      conversationId: number
    }
  | { type: "REMOVE_CONVERSATION"; conversationId: number }
  | { type: "RESET" }

function createEmptySession(
  conversationId: number
): ConversationRuntimeSession {
  return {
    conversationId,
    externalId: null,
    dbConversationId: null,
    detail: null,
    detailLoading: false,
    detailError: null,
    acpLoadError: null,
    localTurns: [],
    backgroundTurns: [],
    pendingBackgroundSettlements: [],
    optimisticTurns: [],
    liveMessage: null,
    syncState: "idle",
    activeTurnToken: null,
    lastTurnOwned: false,
    liveOwnsActiveTurn: false,
    delegationKickoffText: null,
    sessionStats: null,
    delegationActivities: EMPTY_DELEGATION_ACTIVITIES,
    historyAssistantBaseline: null,
    pendingCleanup: false,
    delegateSyncError: null,
    pendingCancel: null,
  }
}

/** Resolve session agent type from loaded detail (conversation summary). */
function resolveSessionAgentType(
  session: ConversationRuntimeSession | null | undefined
): AgentType | null {
  return session?.detail?.summary.agent_type ?? null
}

/**
 * Derive native activities from the last assistant turn's content blocks
 * (historical / promoted local turns). Uses tool_use + paired tool_result.
 */
function deriveActivitiesFromAssistantTurns(
  turns: ReadonlyArray<MessageTurn>,
  agentType: AgentType | null
): DelegationActivityView[] {
  let lastAssistant: MessageTurn | null = null
  for (let i = turns.length - 1; i >= 0; i -= 1) {
    if (turns[i].role === "assistant") {
      lastAssistant = turns[i]
      break
    }
  }
  if (!lastAssistant) return []

  const resultById = new Map<
    string,
    { output: string | null; status: string | null }
  >()
  for (const block of lastAssistant.blocks) {
    if (block.type === "tool_result" && block.tool_use_id) {
      resultById.set(block.tool_use_id, {
        output: block.output_preview ?? null,
        status: block.is_error ? "failed" : "completed",
      })
    }
  }

  const tools: ToolFieldForActivity[] = []
  const at = lastAssistant.timestamp
  for (const block of lastAssistant.blocks) {
    if (block.type !== "tool_use") continue
    const id = block.tool_use_id ?? `anon-${tools.length}`
    const result = block.tool_use_id
      ? resultById.get(block.tool_use_id)
      : undefined
    tools.push({
      toolCallId: id,
      toolName: block.tool_name,
      input: block.input_preview ?? null,
      output: result?.output ?? null,
      status: result?.status ?? "in_progress",
      at,
      meta: block.meta ?? null,
    })
  }
  return deriveNativeActivitiesFromToolCalls(tools, agentType)
}

// Snapshot how many assistant turns are HISTORY when a batch's FIRST turn enters
// the buffers (no local/optimistic turns yet); otherwise keep the batch-start
// value so follow-up prompts in the same batch don't move it. BOTH batch-start
// paths route through this — the owner's own send (APPEND_OPTIMISTIC_TURN) and a
// co-controller's echoed prompt (APPEND_VIEWER_USER_TURN, on every exit incl.
// dedup) — so every disjoint batch that later reaches `syncTurnMetadata` carries
// a boundary; a `null` baseline then means only the overlap paths (e.g. the
// sub-agent adopt, which promotes via COMPLETE_TURN with no user-turn append),
// where the whole parse is this session's.
//
// `promptId` is the id of the prompt starting this batch. Usually `detail` here
// is settled history (owner send / a fresh viewer prompt not yet persisted):
// count every assistant. The ONE exception is a viewer attaching mid-stream,
// where `detail` already holds THIS prompt — and, for OpenCode/Gemini, a PARTIAL
// reply after it — with the backend stamping it as `in_flight_user_turn_id`.
// Only then (marker === promptId) do we cut off at the prompt so the partial
// stays out of history. The marker alone is not enough: it can linger stale
// after completion (see the APPEND_VIEWER content-dedup notes), so trusting it
// for an owner send / distinct prompt would drop a real prior reply from history.
function batchStartHistoryBaseline(
  current: ConversationRuntimeSession,
  promptId: string
): number | null {
  if (current.localTurns.length > 0 || current.optimisticTurns.length > 0) {
    return current.historyAssistantBaseline
  }
  const turns = current.detail?.turns ?? []
  const inFlightId = current.detail?.in_flight_user_turn_id ?? null
  const cutoff =
    inFlightId !== null && inFlightId === promptId
      ? turns.findIndex((t) => t.role === "user" && t.id === inFlightId)
      : -1
  const history = cutoff === -1 ? turns : turns.slice(0, cutoff + 1)
  return history.filter((t) => t.role === "assistant").length
}

interface BuiltStreamingTurns {
  turns: MessageTurn[]
  inProgressToolCallIds: Set<string>
  /**
   * Read-only native/Codeg activity projection derived alongside tool blocks.
   * Never replaces or filters the original `tool_call` content.
   */
  delegationActivities: DelegationActivityView[]
}

// Cache joined chunk output keyed by chunks-array identity. The ACP reducer
// creates a new chunks array only when streaming output actually changes, so
// a WeakMap keyed on the array reference lets repeated renders reuse the
// joined string without re-running O(n) concatenation.
const joinedOutputCache = new WeakMap<readonly string[], string>()

function getJoinedChunks(chunks: readonly string[]): string {
  if (chunks.length === 0) return ""
  if (chunks.length === 1) return chunks[0]
  const cached = joinedOutputCache.get(chunks)
  if (cached !== undefined) return cached
  const joined = chunks.join("")
  joinedOutputCache.set(chunks, joined)
  return joined
}

/**
 * Clean raw Agent tool output that may be JSON or XML wrapped.
 *
 * Streaming Agent results often arrive as raw JSON (e.g. content block
 * arrays from Claude Code, or status wrappers from Codex) or with
 * `<task_result>` XML tags (OpenCode). This function extracts the human-
 * readable text so the Agent card displays clean output.
 */
function cleanAgentOutput(output: string | null): string | null {
  if (!output) return null
  let text = output.trim()
  if (!text) return null

  // Step 1: Unwrap JSON containers (no recursion — single-level unwrap)
  // JSON array of content blocks: [{"type":"text","text":"..."},...]
  if (text.startsWith("[")) {
    try {
      const arr = JSON.parse(text)
      if (Array.isArray(arr)) {
        const texts: string[] = []
        for (const item of arr) {
          if (
            item &&
            typeof item === "object" &&
            typeof item.text === "string"
          ) {
            texts.push(item.text)
          }
        }
        if (texts.length > 0) text = texts.join("\n")
      }
    } catch {
      /* not valid JSON */
    }
  } else if (text.startsWith("{")) {
    // JSON object with common result fields
    try {
      const obj = JSON.parse(text) as Record<string, unknown>
      for (const key of ["result", "output", "text", "content", "completed"]) {
        if (typeof obj[key] === "string") {
          text = (obj[key] as string).trim()
          break
        }
      }
    } catch {
      /* not valid JSON */
    }
  }

  // Step 2: Strip leading session / task_id lines that some agents prepend
  // before the actual result (e.g. "task_id: ses_xxx (for resuming ...)").
  text = text.replace(/^task_id:\s*\S+[^\n]*\n+/, "").trim()
  if (!text) return null

  // Step 3: Extract from <task_result> XML wrapper (OpenCode)
  const tagStart = text.indexOf("<task_result>")
  if (tagStart !== -1) {
    const contentStart = tagStart + "<task_result>".length
    const contentEnd = text.indexOf("</task_result>", contentStart)
    const extracted = text
      .substring(contentStart, contentEnd === -1 ? undefined : contentEnd)
      .trim()
    if (extracted) return extracted
  }

  return text
}

/**
 * Decide whether a live `ToolCallInfo` is codex-acp's image-generation
 * tool call. Detection has to fire during the in-flight window
 * (ImageGenerationBegin, no images yet) so we can't rely on `images.length`
 * alone — the load-bearing signal during that window is the title.
 *
 * Layered detection:
 *   1. `title === "Image generation"` — codex-acp PR #271 hardcodes this
 *      exact English string in `start_image_generation` and
 *      `end_image_generation`. Primary path.
 *   2. Case-insensitive title match — defensive for any future codex-acp
 *      casing/whitespace drift.
 *   3. `images.length > 0` — defensive when title is somehow lost but
 *      images are present (e.g. a snapshot replay that drops the title).
 *
 * The function is intentionally NOT a generic `kind === "other"` matcher
 * because many tools surface as ToolKind::Other.
 */
function isImageGenerationToolCall(info: {
  title?: string | null
  images?: { length: number } | null
}): boolean {
  const title = (info.title ?? "").trim()
  if (title === "Image generation") return true
  if (title.toLowerCase() === "image generation") return true
  return (info.images?.length ?? 0) > 0
}

/**
 * Narrow the wire-typed `ToolCallInfo.status` (declared as `string`) into
 * the strict `ToolCallStatus` union — the reducer only ever stores wire
 * values, but the type system doesn't see that. Anything else falls back
 * to `null` and the renderer treats it as in-flight.
 */
function narrowToolCallStatus(status: string): ToolCallStatus | null {
  switch (status) {
    case "pending":
    case "in_progress":
    case "completed":
    case "failed":
      return status
    default:
      return null
  }
}

/**
 * Strip codex-acp's `"Revised prompt: <text>"` framing from a live
 * `ToolCallInfo.content` string and return the inner text. codex-acp PR #271
 * wraps the codex `revised_prompt` field this way before serialising it as
 * `ToolCallContent::Text` (see `image_generation_content` in codex-acp). The
 * prefix is hardcoded English in upstream, so we match it literally.
 *
 * Returns `null` when content is missing, empty after trimming, or doesn't
 * carry a recognisable revised-prompt frame.
 */
function extractRevisedPrompt(content: string | null): string | null {
  if (!content) return null
  const trimmed = content.trim()
  if (trimmed.length === 0) return null
  const PREFIX = "Revised prompt:"
  if (trimmed.startsWith(PREFIX)) {
    const rest = trimmed.slice(PREFIX.length).trim()
    return rest.length > 0 ? rest : null
  }
  // Fall back to the raw content for unforeseen wrappers (e.g. localized
  // frames in future codex-acp versions). Better to surface something to the
  // user than silently drop it.
  return trimmed
}

/**
 * Fold a Cursor task's wire title ("Task: <description>") into its rawInput
 * as `description`. Applies only when the input carries Cursor's
 * `_toolName:"task"` identity stamp and no description of its own; every
 * other shape returns null (caller falls through to the raw input).
 */
function mergeCursorTaskTitle(
  rawInput: string,
  title: string | null | undefined
): string | null {
  const match = /^task:\s*(\S.*)$/i.exec(title?.trim() ?? "")
  if (!match) return null
  let parsed: unknown
  try {
    parsed = JSON.parse(rawInput)
  } catch {
    return null
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    return null
  }
  const obj = parsed as Record<string, unknown>
  if (obj._toolName !== "task") return null
  if (typeof obj.description === "string" && obj.description.length > 0) {
    return null
  }
  return JSON.stringify({ ...obj, description: match[1].trim() })
}

/** First filesystem path from an ACP tool call's `locations` (`[{ path }]`), or null. */
function firstLocationPath(locations: unknown): string | null {
  if (!Array.isArray(locations)) return null
  for (const loc of locations) {
    if (loc && typeof loc === "object") {
      const p = (loc as { path?: unknown }).path
      if (typeof p === "string" && p.length > 0) return p
    }
  }
  return null
}

function resolveLiveToolInput(
  toolName: string,
  info: ToolCallInfo
): string | null {
  // codex collab tool calls drop the ACP `title` (the op: spawnAgent/wait/
  // closeAgent/…) downstream — only `meta` is forwarded. Merge it back into the
  // rawInput so the card can render an op-aware title. Live-only path; falls
  // through to the raw input when there's no op or it isn't a JSON object.
  if (toolName === COLLAB_AGENT_TOOL_NAME && info.raw_input) {
    const merged = mergeCollabOp(info.raw_input, info.title)
    if (merged) return merged
  }

  // Cursor announces its task tool before the args stream in, so the live
  // rawInput is often just the `{_toolName:"task"}` identity stamp — and the
  // CLI never resends rawInput on later updates. The wire title
  // ("Task: <description>") is the only human-readable label; fold it into
  // the input as `description` so the Agent card doesn't sit on its
  // "starting…" placeholder forever. Never overwrites a real description.
  if (toolName === "agent" && info.raw_input) {
    const merged = mergeCursorTaskTitle(info.raw_input, info.title)
    if (merged) return merged
  }

  if (info.raw_input && info.raw_input.trim().length > 0) {
    return info.raw_input
  }

  // codex classifies file-reading shell commands (sed/cat/head) as ACP `read`
  // commandActions: kind="read", the path only in `locations`/`title`, and NO
  // raw_input (codex-acp createCommandActionEvent). Synthesize a `file_path` from
  // the location so the read card derives a "Read <path>" title + file view,
  // matching a normal read instead of falling back to "read: <output>".
  if (toolName === "read") {
    const path = firstLocationPath(info.locations)
    if (path) return JSON.stringify({ file_path: path })
  }

  const goal = parseGoalUpdateTitle(info.title)
  if (!goal) return info.raw_input

  if (toolName === "create_goal") {
    return JSON.stringify({ objective: goal.objective })
  }
  if (toolName === "update_goal") {
    return JSON.stringify({
      status: goal.status,
      objective: goal.objective,
    })
  }

  return info.raw_input
}

export function buildStreamingTurnsFromLiveMessage(
  conversationId: number,
  liveMessage: LiveMessage,
  options?: { agentType?: AgentType | null }
): BuiltStreamingTurns {
  // Consolidate codex collab capsules first (spawn execution + per-wait result,
  // close folded in) so live matches the history reconstruction. No-op when the
  // message has no collab tool calls. See collab-collapse.ts.
  const content = collapseLiveCollabBlocks(liveMessage.content)

  // Kimi Code live TodoList handling. Kimi emits BOTH a `TodoList` tool_call AND
  // a canonical `plan` update for each write; the reducer collapses all plan
  // updates into a single latest `plan` block. `hasLivePlan` gates the one-frame
  // pre-plan fallback in the tool_call case. `latestKimiTodoEntries` is the
  // canonical plan content: the LAST Kimi write's todos, which are always as
  // fresh as — and, in the one-event window before that write's own `plan`
  // update lands, fresher than — the (collapsed) plan block. Rendering the plan
  // block from these avoids showing the previous write's stale plan during that
  // window. Null for non-Kimi sessions (e.g. Claude Code), where the synthetic
  // plan block is the sole source and is used verbatim.
  const hasLivePlan = content.some((block) => block.type === "plan")
  let latestKimiTodoEntries: PlanEntryInfo[] | null = null
  for (const block of content) {
    if (block.type === "tool_call") {
      const entries = kimiTodoWriteEntries(block.info.raw_input)
      if (entries) latestKimiTodoEntries = entries
    }
  }

  // ── Phase 1: Identify agent → child relationships ──────────────────
  // Uses meta.claudeCode.parentToolUseId when available (precise), with
  // position-based fallback for agents that don't provide it.
  const agentChildren = new Map<
    string,
    Array<{ info: ToolCallInfo; toolName: string }>
  >()
  const childToolCallIds = new Set<string>()

  // Cache inferred tool names — inferLiveToolName is called per tool_call
  // in both Phase 1 and Phase 2; caching avoids redundant computation.
  const inferredNames = new Map<string, string>()
  const getToolName = (info: ToolCallInfo): string => {
    const cached = inferredNames.get(info.tool_call_id)
    if (cached !== undefined) return cached
    const name = inferLiveToolName({
      title: info.title,
      kind: info.kind,
      rawInput: info.raw_input,
      meta: info.meta,
    })
    inferredNames.set(info.tool_call_id, name)
    return name
  }

  // First pass: register all agent tool_call IDs
  const agentIds = new Set<string>()
  for (const block of content) {
    if (block.type !== "tool_call") continue
    if (getToolName(block.info) === "agent") {
      agentIds.add(block.info.tool_call_id)
      agentChildren.set(block.info.tool_call_id, [])
    }
  }

  // Second pass: assign children using parentToolUseId or position fallback.
  // Positional fallback only captures while the agent is still in-progress;
  // once it completes/fails, subsequent tool calls are treated as top-level.
  let positionalAgentId: string | null = null

  for (const block of content) {
    if (block.type === "tool_call") {
      const toolName = getToolName(block.info)

      if (toolName === "agent") {
        const isFinal =
          block.info.status === "completed" || block.info.status === "failed"
        // Only capture children while the agent is still running
        positionalAgentId = isFinal ? null : block.info.tool_call_id
      } else {
        // Extract the parent tool-call id from ACP meta. Both Claude Code and
        // CodeBuddy link a native sub-agent's child tool calls to the parent
        // Agent tool call this way — just under different keys:
        //   - Claude Code: nested `meta.claudeCode.parentToolUseId`
        //   - CodeBuddy:   flat  `meta["codebuddy.ai/parentToolCallId"]`
        // Reading both makes CodeBuddy sub-agents nest precisely (like Claude
        // Code) instead of falling back to the positional heuristic, which the
        // sub-agent's interleaved thinking blocks break. Guard each access level
        // to avoid crashes on unexpected shapes from other agents.
        const meta = block.info.meta
        let parentId: string | undefined
        if (meta && typeof meta === "object") {
          if ("claudeCode" in meta) {
            const cc = (meta as Record<string, unknown>).claudeCode
            if (cc && typeof cc === "object" && "parentToolUseId" in cc) {
              const pid = (cc as Record<string, unknown>).parentToolUseId
              if (typeof pid === "string") parentId = pid
            }
          }
          if (!parentId) {
            const cbParent = (meta as Record<string, unknown>)[
              "codebuddy.ai/parentToolCallId"
            ]
            if (typeof cbParent === "string") parentId = cbParent
          }
        }

        // Use explicit parent id when available, positional fallback only for
        // in-progress agents.
        const resolvedParent =
          parentId && agentIds.has(parentId) ? parentId : positionalAgentId

        if (resolvedParent) {
          childToolCallIds.add(block.info.tool_call_id)
          agentChildren
            .get(resolvedParent)
            ?.push({ info: block.info, toolName })
        }
      }
    } else if (positionalAgentId) {
      // A non-tool block (text/thinking/plan) means the main agent is
      // producing new content — stop position-based capture.
      positionalAgentId = null
    }
  }

  // ── Phase 2: Build turns, nesting children inside Agent results ────
  // Split streaming content into multiple turns matching the historical
  // pattern: each "round" (text/thinking + tool calls + tool results) is a
  // separate turn. A new turn starts when a text/thinking/plan block appears
  // after completed tool calls in the current group.
  const groups: MessageTurn["blocks"][] = [[]]
  let currentGroupHasCompletedTool = false
  const inProgressToolCallIds = new Set<string>()

  for (const block of content) {
    const isContentBlock =
      block.type === "text" ||
      block.type === "thinking" ||
      block.type === "plan"

    if (isContentBlock && currentGroupHasCompletedTool) {
      groups.push([])
      currentGroupHasCompletedTool = false
    }

    const currentBlocks = groups[groups.length - 1]

    switch (block.type) {
      case "text":
        if (block.text.length > 0) {
          currentBlocks.push({ type: "text", text: block.text })
        }
        break
      case "thinking":
        // Keep empty thinking blocks during streaming so the reasoning UI
        // can show its "Thinking..." indicator before any reasoning text
        // arrives (and for newer Claude models that redact reasoning text
        // entirely while still emitting thinking blocks).
        currentBlocks.push({ type: "thinking", text: block.text })
        break
      case "plan": {
        // Carry the live plan through as a first-class `plan` block so it
        // renders in a dedicated <PlanCard> instead of being down-converted
        // into a `thinking`/reasoning block. For a Kimi Code session, prefer the
        // latest TodoList write's todos over this (possibly one-event-stale)
        // collapsed plan, so a new write's content shows immediately rather than
        // the previous write's until Kimi's own `plan` update catches up.
        currentBlocks.push({
          type: "plan",
          entries: latestKimiTodoEntries ?? block.entries,
        })
        break
      }
      case "tool_call": {
        // Skip child tool calls — they are nested inside Agent cards
        if (childToolCallIds.has(block.info.tool_call_id)) break

        // codex-acp v0.14+ image generation surfaces as a `ToolCall` whose
        // ACP-wire shape is `(title="Image generation", kind=Other,
        // content=[Text("Revised prompt: ..."), Image{...}])`. Render this
        // as a dedicated `image_generation` block instead of the generic
        // tool_use + tool_result pair so:
        //   - live and historical (JSONL) paths converge on the same
        //     ContentBlock variant (zero asymmetry)
        //   - the user sees one labeled "Image generation" card instead of
        //     a generic tool card sitting above a detached image
        //   - the new card is not folded into `groupConsecutiveToolCalls`
        //     (which only consumes `tool-call` parts)
        if (isImageGenerationToolCall(block.info)) {
          // codex-acp emits one image per ToolCall (each `call_id` is a
          // single ImageGenerationBegin/End pair). One block per image —
          // multiple images in a turn become multiple consecutive blocks.
          // Defensive fallback: if a future agent ever sends multiple
          // images in one ToolCall, we still emit one block per image so
          // each renders as its own card.
          const imgs = block.info.images ?? []
          const revisedPrompt = extractRevisedPrompt(block.info.content)
          // Live ToolCallStatus is forwarded so the renderer can show a
          // failure slot when codex reports the call failed before any
          // image bytes arrived. Without this the in-flight skeleton would
          // sit there until TurnComplete clears `active_tool_calls`.
          const status = narrowToolCallStatus(block.info.status)
          if (imgs.length === 0) {
            // In-flight placeholder: title arrived, image hasn't (or the
            // call failed without producing one).
            currentBlocks.push({
              type: "image_generation",
              revised_prompt: revisedPrompt,
              image: null,
              status,
            })
          } else {
            for (const img of imgs) {
              currentBlocks.push({
                type: "image_generation",
                revised_prompt: revisedPrompt,
                image: {
                  data: img.data,
                  mime_type: img.mime_type,
                  uri: img.uri ?? null,
                },
                status,
              })
            }
          }
          if (status === "completed" || status === "failed") {
            currentGroupHasCompletedTool = true
          }
          break
        }

        // Kimi Code emits BOTH a `TodoList` tool_call AND a canonical `plan`
        // update for the same write (claude-code-acp instead replaces the
        // tool_call with the plan). Render exactly one <PlanCard> with no
        // duplicate-card flash: once Kimi's own `plan` block is in this live
        // message, drop the redundant tool card (the synthetic plan renders it);
        // in the one-frame window before that plan block arrives (the tool_call
        // is processed one event earlier), render the plan from the call's own
        // todos so a PlanCard — never a generic tool card — shows immediately and
        // continues seamlessly (identical entries) when the synthetic block takes
        // over. Fail-safe: if Kimi never emitted the plan, the converted plan
        // still shows (no data loss). Identity is the exact Kimi todo-write input
        // shape because the real tool name "TodoList" is never on the live wire.
        const kimiTodos = kimiTodoWriteEntries(block.info.raw_input)
        if (kimiTodos) {
          if (!hasLivePlan) {
            currentBlocks.push({ type: "plan", entries: kimiTodos })
          }
          // `break` precedes the tool_use and tool_result pushes — no orphan
          // tool-result and no generic tool card for the suppressed write.
          break
        }

        const toolName = getToolName(block.info)
        currentBlocks.push({
          type: "tool_use",
          tool_use_id: block.info.tool_call_id,
          tool_name: toolName,
          input_preview: resolveLiveToolInput(toolName, block.info),
          // Forward the ACP `meta` field downstream so the renderer can
          // read delegation state (`meta["codeg.delegation"]`) for
          // pre-binding / post-refresh fallback rendering of
          // <DelegatedSubThread>. Opaque pass-through — adapter layer
          // does not interpret.
          meta: block.info.meta,
        })
        const isFinalState =
          block.info.status === "completed" || block.info.status === "failed"
        // Output precedence: raw_output_chunks (terminal polling / SDK
        // raw_output field) wins over content. Some agents stream bash output
        // via raw_output with raw_output_append, others via content-only
        // tool_call_update notifications — we support both.
        const resolvedOutput =
          block.info.raw_output_chunks.length > 0
            ? getJoinedChunks(block.info.raw_output_chunks)
            : block.info.content

        // For agent tool calls, build agent_stats from collected children
        const isAgent = toolName === "agent"
        const children = isAgent
          ? (agentChildren.get(block.info.tool_call_id) ?? [])
          : []

        const agentStats: AgentExecutionStats | undefined =
          isAgent && children.length > 0
            ? {
                tool_calls: children.map(({ info: ci, toolName: cn }) => {
                  const cFinal =
                    ci.status === "completed" || ci.status === "failed"
                  const cOutput =
                    ci.raw_output_chunks.length > 0
                      ? getJoinedChunks(ci.raw_output_chunks)
                      : ci.content
                  return {
                    tool_name: cn,
                    input_preview: ci.raw_input?.substring(0, 500) ?? null,
                    output_preview: cFinal
                      ? (cOutput?.substring(0, 500) ?? null)
                      : null,
                    is_error: ci.status === "failed",
                  }
                }),
              }
            : undefined

        if (isFinalState) {
          currentBlocks.push({
            type: "tool_result",
            tool_use_id: block.info.tool_call_id,
            output_preview: isAgent
              ? cleanAgentOutput(resolvedOutput)
              : resolvedOutput,
            is_error: block.info.status === "failed",
            ...(agentStats ? { agent_stats: agentStats } : {}),
          })
          currentGroupHasCompletedTool = true
        } else if (resolvedOutput || (isAgent && children.length > 0)) {
          // In-progress tool that already produced partial output (or an
          // agent with child calls). Emit the running result so the renderer
          // can display live output / nested tool calls, and flag the
          // tool_call so the adapter keeps state="input-available".
          //
          // For Agents specifically, partial `content` from Claude Code's
          // Task tool echoes the prompt (and subagent message fragments)
          // before the real result arrives — suppress it so the Agent card
          // doesn't duplicate the prompt already shown in the collapsible.
          currentBlocks.push({
            type: "tool_result",
            tool_use_id: block.info.tool_call_id,
            output_preview: isAgent ? null : (resolvedOutput ?? null),
            is_error: false,
            ...(agentStats ? { agent_stats: agentStats } : {}),
          })
          inProgressToolCallIds.add(block.info.tool_call_id)
        }
        break
      }
    }
  }

  const timestamp = new Date(liveMessage.startedAt).toISOString()
  const turns = groups
    .filter((blocks) => blocks.length > 0)
    .map((blocks, i) => ({
      id:
        i === 0
          ? `live-${conversationId}-${liveMessage.id}`
          : `live-${conversationId}-${liveMessage.id}-${i}`,
      role: "assistant" as const,
      blocks,
      timestamp,
    }))

  // Derive native activity alongside tool blocks — never filter/replace them.
  // Use the pre-collapse tool stream so Codex collab ops keep their native
  // names (`spawn_agent` / `wait_agent` / …) rather than the collapsed
  // collab capsule title. Source tool blocks in `turns` remain unchanged.
  const activityToolInputs: Array<{
    toolCallId: string
    toolName: string
    input?: string | null
    output?: string | null
    status?: string | null
    at?: string
  }> = []
  for (const block of liveMessage.content) {
    if (block.type !== "tool_call") continue
    const resolvedOutput =
      block.info.raw_output_chunks.length > 0
        ? getJoinedChunks(block.info.raw_output_chunks)
        : (block.info.content ?? null)
    // Prefer ACP title (spawn_agent / TaskOutput / …) so native mapping
    // sees the platform tool identity before freeform name collapse.
    const title = block.info.title?.trim() || ""
    const inferred = inferLiveToolName({
      title: block.info.title,
      kind: block.info.kind,
      rawInput: block.info.raw_input,
      meta: block.info.meta,
    })
    activityToolInputs.push({
      toolCallId: block.info.tool_call_id,
      toolName: title || inferred,
      input: block.info.raw_input ?? null,
      output: resolvedOutput,
      status: block.info.status ?? null,
      at: timestamp,
    })
  }
  const delegationActivities = deriveNativeActivitiesFromToolCalls(
    activityToolInputs,
    options?.agentType ?? null
  )

  return { turns, inProgressToolCallIds, delegationActivities }
}

/**
 * Build streaming turns with session agentType resolved from detail when the
 * caller does not pass an explicit hint.
 */
function buildStreamingTurnsForSession(
  session: ConversationRuntimeSession,
  liveMessage: LiveMessage,
  agentTypeOverride?: AgentType | null
): BuiltStreamingTurns {
  const agentType =
    agentTypeOverride !== undefined
      ? agentTypeOverride
      : resolveSessionAgentType(session)
  return buildStreamingTurnsFromLiveMessage(
    session.conversationId,
    liveMessage,
    {
      agentType,
    }
  )
}

/** Metadata backfilled onto a local assistant turn by the post-turn reparse. */
export interface TurnMetadataPatch {
  index: number
  usage?: TurnUsage | null
  duration_ms?: number | null
  model?: string | null
  completed_at?: string | null
  tool_meta?: Array<{
    tool_use_id: string
    meta: Record<string, unknown> | null
  }>
}

/**
 * Align a fresh parse's assistant turns onto this session's completed local
 * assistant turns and emit the metadata (usage / duration / model /
 * completed_at) to backfill onto each.
 *
 * The subtlety is history. `localTurns` holds ONLY turns completed in the
 * current session; persisted history lives in `detail`. The fresh parse
 * returns history + this session's turns in order, so the local turns line up
 * with the parse TAIL, past the `persistedAssistantCount` historical turns —
 * hence the slice below before any offset math.
 *
 * That boundary is also what makes the "fold extra parser sub-turns into
 * local[0]" step correct. When the parser splits the current reply into MORE
 * sub-turns than the live stream did, the leading unmatched SESSION turns are
 * genuine sub-turns of local[0] and their stats must be summed in (so the
 * post-stream total matches a fresh reload). Folding the FULL parse instead —
 * the original bug — summed every historical turn's duration/usage into the
 * first reply after resuming a conversation; because {@link
 * conversationRuntimeReducer}'s PATCH_TURN_METADATA is first-write-wins, that
 * wrong value then stuck until a full reload cleared localTurns and rendered
 * each parsed turn directly.
 *
 * A parse that hasn't caught up yet (fewer session turns than local) head-
 * aligns the turns it does have and leaves the rest unpatched, so a later
 * local reply never inherits an earlier one's stats and the caller's retry can
 * pick up the complete parse rather than lock in a stale value.
 *
 * `persistedAssistantCount` is the caller's send-time history baseline (see the
 * session's `historyAssistantBaseline`), so it is immune to a mid-stream detail
 * refetch folding this session's own partial into `detail`. `0` treats the
 * whole parse as this session's — correct when `localTurns` overlaps the parse
 * tail (the sub-agent adopt path) and identical to the pre-slice behavior.
 */
export function computeTurnMetadataPatches(params: {
  localAssistantIndices: number[]
  parsedAssistantTurns: MessageTurn[]
  persistedAssistantCount: number
}): TurnMetadataPatch[] {
  const { localAssistantIndices, parsedAssistantTurns } = params
  // Drop the persisted history at the front of the parse; only this session's
  // turns can align to localTurns. Clamp so a detail/parse count skew (e.g. a
  // transient in-flight partial in `detail`) can't slice past the end.
  const historyBoundary = Math.min(
    Math.max(params.persistedAssistantCount, 0),
    parsedAssistantTurns.length
  )
  const sessionParsedTurns = parsedAssistantTurns.slice(historyBoundary)

  const offset = sessionParsedTurns.length - localAssistantIndices.length
  const patches: TurnMetadataPatch[] = []

  for (let i = 0; i < localAssistantIndices.length; i++) {
    // Tail-align local turns to the session parse so a sub-turn split (parser
    // emits MORE turns than the live stream did) folds its leading extras into
    // local[0]. When the parse hasn't caught up to every local turn yet
    // (offset < 0), `Math.max(offset, 0)` head-aligns instead: it maps the
    // turns that ARE parsed onto the earliest locals and leaves the rest
    // unpatched, rather than shifting a parsed turn onto a LATER local reply —
    // which, with first-write-wins metadata, would lock a wrong value there.
    const parsedIdx = Math.max(offset, 0) + i
    let usageToApply: TurnUsage | null | undefined
    let durationToApply: number | null | undefined
    let modelToApply: string | null | undefined
    // For the merged-sub-turn case (offset > 0), the latest completion is
    // sessionParsedTurns[parsedIdx] (the sub-turn we matched); earlier
    // rolled-in parsed turns precede it in time, so we don't aggregate
    // completion timestamps.
    let completedAtToApply: string | null | undefined

    if (parsedIdx >= 0 && parsedIdx < sessionParsedTurns.length) {
      const pt = sessionParsedTurns[parsedIdx]
      usageToApply = pt.usage
      durationToApply = pt.duration_ms
      modelToApply = pt.model
      completedAtToApply = pt.completed_at
    }

    // When the parser splits the response into more sub-turns than the live
    // stream did (offset > 0), roll the leading unmatched SESSION turns'
    // usage/duration into local[0] so that sum(local) equals sum(parsed) for
    // this session. `sessionParsedTurns` already excludes history, so this
    // never folds an older turn's stats in.
    if (i === 0 && offset > 0) {
      for (let j = 0; j < offset; j++) {
        const extra = sessionParsedTurns[j]
        if (extra.usage) {
          if (!usageToApply) {
            usageToApply = { ...extra.usage }
          } else {
            usageToApply = {
              input_tokens:
                usageToApply.input_tokens + extra.usage.input_tokens,
              output_tokens:
                usageToApply.output_tokens + extra.usage.output_tokens,
              cache_creation_input_tokens:
                usageToApply.cache_creation_input_tokens +
                extra.usage.cache_creation_input_tokens,
              cache_read_input_tokens:
                usageToApply.cache_read_input_tokens +
                extra.usage.cache_read_input_tokens,
            }
          }
        }
        if (typeof extra.duration_ms === "number") {
          durationToApply = (durationToApply ?? 0) + extra.duration_ms
        }
        if (!modelToApply && extra.model) {
          modelToApply = extra.model
        }
      }
    }

    if (
      !usageToApply &&
      !durationToApply &&
      !modelToApply &&
      !completedAtToApply
    )
      continue
    patches.push({
      index: localAssistantIndices[i],
      usage: usageToApply,
      duration_ms: durationToApply,
      model: modelToApply,
      completed_at: completedAtToApply,
    })
  }

  return patches
}

function upsertExternalIdIndex(
  index: Map<string, number>,
  previousExternalId: string | null,
  nextExternalId: string | null,
  conversationId: number
): Map<string, number> {
  const next = new Map(index)
  if (previousExternalId) {
    next.delete(previousExternalId)
  }
  if (nextExternalId) {
    next.set(nextExternalId, conversationId)
  }
  return next
}

function updateSessionInState(
  state: ConversationRuntimeState,
  conversationId: number,
  updater: (current: ConversationRuntimeSession) => ConversationRuntimeSession
): ConversationRuntimeState {
  const current =
    state.byConversationId.get(conversationId) ??
    createEmptySession(conversationId)
  const nextSession = updater(current)
  const nextByConversationId = new Map(state.byConversationId)
  nextByConversationId.set(conversationId, nextSession)
  return { ...state, byConversationId: nextByConversationId }
}

/**
 * Stable content signature for a USER turn. The same prompt surfaces under two
 * unrelated id namespaces: a cross-client viewer's synthesized turn uses the
 * broadcast `message_id`, while the SAME prompt, once the agent has written it
 * to its JSONL transcript, comes back from the parser (in `detail.turns`) under
 * a parser-assigned id. Id-based dedup therefore can't recognize the two as one
 * message — only content can. Ids and timestamps are deliberately excluded.
 * The encoding is structurally unambiguous (JSON, so block boundaries can't
 * collide) and compares FULL payload — text verbatim and full image data — so a
 * genuinely different prompt is never mistaken for a match. That matters because
 * a match SUPPRESSES a visible user turn (see `APPEND_VIEWER_USER_TURN`), and it
 * runs only on the rare cross-client viewer append, so comparing full data is
 * fine. Unknown block types are serialized whole rather than collapsed to their
 * tag, so no distinguishing content is silently dropped.
 */
function userTurnContentKey(turn: MessageTurn): string {
  return JSON.stringify(
    turn.blocks.map((b) => {
      switch (b.type) {
        case "text":
          return { t: b.text }
        case "image":
          return { i: b.mime_type, d: b.data }
        default:
          return b
      }
    })
  )
}

function sameJsonValue(a: unknown, b: unknown): boolean {
  if (a === b) return true
  return JSON.stringify(a ?? null) === JSON.stringify(b ?? null)
}

function collectToolMetaPatches(
  localTurn: MessageTurn,
  parsedTurns: MessageTurn[]
): Array<{ tool_use_id: string; meta: Record<string, unknown> | null }> {
  const localToolUseIds = new Set<string>()
  for (const block of localTurn.blocks) {
    if (block.type === "tool_use" && block.tool_use_id) {
      localToolUseIds.add(block.tool_use_id)
    }
  }
  if (localToolUseIds.size === 0) return []

  const byId = new Map<string, Record<string, unknown> | null>()
  for (const turn of parsedTurns) {
    for (const block of turn.blocks) {
      if (
        block.type === "tool_use" &&
        block.tool_use_id &&
        localToolUseIds.has(block.tool_use_id) &&
        block.meta !== undefined
      ) {
        byId.set(block.tool_use_id, block.meta ?? null)
      }
    }
  }

  const patches: Array<{
    tool_use_id: string
    meta: Record<string, unknown> | null
  }> = []
  for (const block of localTurn.blocks) {
    if (
      block.type === "tool_use" &&
      block.tool_use_id &&
      byId.has(block.tool_use_id)
    ) {
      const parsedMeta = byId.get(block.tool_use_id) ?? null
      if (!sameJsonValue(block.meta ?? null, parsedMeta)) {
        patches.push({ tool_use_id: block.tool_use_id, meta: parsedMeta })
      }
    }
  }
  return patches
}

/**
 * Rewrite the launching tool call's `[[codeg-background-task]]` marker in a turn
 * list so `AgentToolCallPart` flips from "running in background" to its
 * completed/result form — the same marker shape the disk parser
 * (`apply_background_lifecycle`) produces, so live and cold-open render
 * identically. Locates the `tool_result` block by `toolUseId` (how the adapter's
 * `buildToolResultMap` pairs the card).
 *
 * Returns `matched` (a block with this `toolUseId` exists here) SEPARATELY from
 * `changed` (its `output_preview` was actually rewritten). The distinction is
 * load-bearing: a settlement whose card is already showing exactly this result
 * is `matched` but not `changed` — callers must treat it as handled (NOT queue
 * it), or an idempotent re-settle would be buffered and later re-applied over a
 * newer result. `turns` keeps its original reference when nothing changed.
 */
function applyBackgroundSettlementToTurns(
  turns: MessageTurn[],
  settlement: PendingBackgroundSettlement
): { turns: MessageTurn[]; matched: boolean; changed: boolean } {
  const marker =
    BACKGROUND_TASK_MARKER +
    JSON.stringify({
      task_id: settlement.taskId,
      status: settlement.status,
      summary: settlement.summary,
      result: settlement.result,
    })
  let matched = false
  let changed = false
  const nextTurns = turns.map((turn) => {
    let turnChanged = false
    const nextBlocks = turn.blocks.map((block) => {
      if (
        block.type === "tool_result" &&
        block.tool_use_id === settlement.toolUseId
      ) {
        matched = true
        if (block.output_preview !== marker) {
          turnChanged = true
          changed = true
          return { ...block, output_preview: marker }
        }
      }
      return block
    })
    return turnChanged ? { ...turn, blocks: nextBlocks } : turn
  })
  return { turns: changed ? nextTurns : turns, matched, changed }
}

function reducer(
  state: ConversationRuntimeState,
  action: Action
): ConversationRuntimeState {
  switch (action.type) {
    case "FETCH_DETAIL_START":
      return updateSessionInState(state, action.conversationId, (current) => ({
        ...current,
        detailLoading: true,
        detailError: null,
      }))

    case "FETCH_DETAIL_SUCCESS": {
      const current =
        state.byConversationId.get(action.conversationId) ??
        createEmptySession(action.conversationId)
      const nextExternalId = action.detail.summary.external_id ?? null

      // DB data is authoritative for completed turns. Normally clear all the
      // in-flight buffers (localTurns/optimisticTurns/liveMessage). Preserve
      // them when the user actively sent a message and is awaiting the agent
      // response (awaiting_persist), OR the caller asked to keep the live state
      // via `preserveLive` (the sub-agent dialog, folding the persisted user
      // kickoff in while the child still streams/just-finished its reply — the
      // bridged/promoted reply must outlive the fetch so a late partial can't
      // momentarily replace it).
      //
      // A detail that carries `in_flight_user_turn_id` is itself a MID-TURN
      // snapshot (the backend only stamps it while a turn is running). Such a
      // response must not clobber a more-complete live/promoted reply: a stale one
      // landing just after `completeTurn` promoted the reply would otherwise clear
      // `localTurns`, and the next live turn's in-flight suppression (keyed off the
      // stale id) could then hide that completed reply. So treat it like
      // `preserveLive` and keep every live buffer; a settled (non-in-flight) load
      // replaces them authoritatively.
      const detailIsInFlight = action.detail.in_flight_user_turn_id != null
      const isActivelyInteracting =
        current.syncState === "awaiting_persist" ||
        action.preserveLive === true ||
        detailIsInFlight
      const keepAllLiveBuffers =
        action.preserveLive === true || detailIsInFlight

      // Retire overlay turns the refetched detail now covers: both sides
      // measure byte offsets of the SAME transcript, so `entry.watermark <=
      // detail.transcript_watermark` means this detail literally contains
      // those bytes. Entries beyond the detail's watermark stay (they were
      // parsed from bytes appended after this fetch read the file). A detail
      // without a watermark (non-Claude parser) retires nothing — its overlay
      // is never populated anyway.
      const detailWatermark = action.detail.transcript_watermark ?? null
      const retainedBackground =
        detailWatermark === null
          ? current.backgroundTurns
          : current.backgroundTurns.filter((e) => e.watermark > detailWatermark)
      const nextBackgroundTurns =
        retainedBackground.length === current.backgroundTurns.length
          ? current.backgroundTurns
          : retainedBackground

      const nextSessionBase: ConversationRuntimeSession = {
        ...current,
        detail: action.detail,
        detailLoading: false,
        detailError: null,
        delegateSyncError: null,
        externalId: nextExternalId ?? current.externalId,
        sessionStats: action.detail.session_stats ?? current.sessionStats,
        backgroundTurns: nextBackgroundTurns,
        ...(isActivelyInteracting
          ? keepAllLiveBuffers
            ? {}
            : { localTurns: [] }
          : { localTurns: [], optimisticTurns: [], liveMessage: null }),
      }
      // When live buffers are cleared, re-derive activities from the last
      // assistant turn in detail (+ any remaining localTurns). While live is
      // preserved, keep existing store activities (live path owns them).
      const agentType = action.detail.summary.agent_type
      let delegationActivities = nextSessionBase.delegationActivities
      if (!isActivelyInteracting || !keepAllLiveBuffers) {
        const sourceTurns = [
          ...action.detail.turns,
          ...nextSessionBase.localTurns,
        ]
        delegationActivities = deriveActivitiesFromAssistantTurns(
          sourceTurns,
          agentType
        )
      } else if (
        nextSessionBase.liveMessage &&
        nextSessionBase.delegationActivities.length === 0
      ) {
        delegationActivities = buildStreamingTurnsForSession(
          nextSessionBase,
          nextSessionBase.liveMessage,
          agentType
        ).delegationActivities
      }
      const nextSession: ConversationRuntimeSession = {
        ...nextSessionBase,
        delegationActivities,
      }

      const nextByConversationId = new Map(state.byConversationId)
      nextByConversationId.set(action.conversationId, nextSession)
      const nextExternalIndex = upsertExternalIdIndex(
        state.conversationIdByExternalId,
        current.externalId,
        nextExternalId ?? current.externalId,
        action.conversationId
      )

      return {
        byConversationId: nextByConversationId,
        conversationIdByExternalId: nextExternalIndex,
      }
    }

    case "FETCH_DETAIL_ERROR":
      return updateSessionInState(state, action.conversationId, (current) => ({
        ...current,
        detailLoading: false,
        detailError: action.error,
      }))

    case "SET_DELEGATE_SYNC_ERROR":
      return updateSessionInState(state, action.conversationId, (current) => ({
        ...current,
        delegateSyncError: action.error,
      }))

    case "COMPLETE_TURN": {
      const current = state.byConversationId.get(action.conversationId)
      if (!current) return state

      // Idempotency guard — a single turn can be promoted twice when the
      // panel's connStatus-edge effect and ConversationDetailPanel's
      // background turn_complete listener both fire (e.g. when the bg
      // listener's tab-membership check misses the new-conversation race
      // and proceeds). The first call drains liveMessage + optimisticTurns
      // into localTurns and lands syncState=idle; a second pass with a
      // caller-provided action.liveMessage would otherwise rebuild
      // streamingTurns from action.liveMessage and append them on top of
      // the already-promoted turns, producing a duplicated assistant
      // message in the timeline. If the session is already drained, the
      // turn is a no-op regardless of action.liveMessage.
      if (
        current.liveMessage === null &&
        current.optimisticTurns.length === 0 &&
        current.syncState === "idle"
      ) {
        // Surface the unexpected double-invocation so future regressions
        // are noticed in the console rather than silently swallowed.
        // Reaching this branch means an upstream guard (e.g. the bg
        // listener's tab-membership check) failed to dedupe.
        console.warn(
          "[conversation-runtime] COMPLETE_TURN dispatched on an already-drained session; ignoring",
          { conversationId: action.conversationId }
        )
        return state
      }

      // Prefer the caller-provided liveMessage when present. `current.liveMessage`
      // is kept in sync by the connection dispatch's liveMessage sink (see
      // `registerLiveMessageSink` in the connections context), which writes
      // synchronously as each batch is applied — so by turn-end it already holds
      // the final chunk. The conversation panel omits it (it no longer subscribes
      // to conn.liveMessage) and relies on this sink-synced fallback; the
      // background turn_complete listener likewise passes nothing. The sub-agent
      // dialog's child bridge still passes its liveMessage explicitly.
      const sourceLiveMessage =
        action.liveMessage !== undefined
          ? action.liveMessage
          : current.liveMessage

      // Convert liveMessage to completed MessageTurns (split into rounds).
      // Always pass session agentType so Agent/Task disambiguates correctly.
      const built = sourceLiveMessage
        ? buildStreamingTurnsForSession(current, sourceLiveMessage)
        : null
      const streamingTurns = built?.turns ?? []
      const delegationActivities =
        built && built.delegationActivities.length > 0
          ? built.delegationActivities
          : EMPTY_DELEGATION_ACTIVITIES

      // Promote: optimisticTurns + streamingTurns → localTurns. Dedup by turn
      // id (keep the latest copy) so a re-promotion of an already-promoted turn
      // doesn't leave two same-id turns in `localTurns`. This happens when the
      // background `turn_complete` listener races the panel's own promotion
      // after the same liveMessage was re-bridged: the first COMPLETE_TURN puts
      // a snapshot into localTurns, the live turn re-streams under the same id,
      // and a second COMPLETE_TURN would append it again. Identical ids mean the
      // same underlying turn, so the later (most complete) copy supersedes.
      const promotedRaw = [
        ...current.localTurns,
        ...current.optimisticTurns,
        ...streamingTurns,
      ]
      const promotedLastIndexById = new Map<string, number>()
      promotedRaw.forEach((turn, i) => promotedLastIndexById.set(turn.id, i))
      const promotedDeduped =
        promotedLastIndexById.size === promotedRaw.length
          ? promotedRaw
          : promotedRaw.filter(
              (turn, i) => promotedLastIndexById.get(turn.id) === i
            )

      // Drain queued async-sub-agent settlements against the just-promoted
      // turns: a task that settled while this turn was still held open (#870)
      // couldn't flip its launch card then (the tool call was in `liveMessage`,
      // un-patchable); now it's in `promoted`. Apply each, keep the ones that
      // still don't match (their launch turn belongs to a different, not-yet-
      // promoted turn — or never will, e.g. an abandoned turn — leaving the card
      // no worse off than before, and bounded to this small buffer).
      let promoted = promotedDeduped
      let remainingSettlements = current.pendingBackgroundSettlements
      if (current.pendingBackgroundSettlements.length > 0) {
        const stillPending: PendingBackgroundSettlement[] = []
        for (const settlement of current.pendingBackgroundSettlements) {
          const res = applyBackgroundSettlementToTurns(promoted, settlement)
          // Consume on `matched` (the block surfaced), not just `changed`: if
          // the promoted card already shows this result, the entry is still
          // handled and must not linger to be re-applied later.
          if (res.matched) {
            promoted = res.turns
          } else {
            stillPending.push(settlement)
          }
        }
        remainingSettlements =
          stillPending.length === current.pendingBackgroundSettlements.length
            ? current.pendingBackgroundSettlements
            : stillPending
      }

      return updateSessionInState(state, action.conversationId, () => ({
        ...current,
        localTurns: promoted,
        optimisticTurns: [],
        liveMessage: null,
        syncState: "idle",
        activeTurnToken: null,
        // Persist projected activities for overlay consumers (I3).
        delegationActivities,
        // Capture WHO drove this turn before `syncState` collapses to `idle`:
        // an owner send is `awaiting_persist`, a viewer's watched turn is not.
        // `isPureViewerSession` uses this to keep an owner's possibly-unflushed
        // reply out of viewer-sync while still admitting a viewer whose promoted
        // reply is already persisted.
        lastTurnOwned: current.syncState === "awaiting_persist",
        pendingBackgroundSettlements: remainingSettlements,
      }))
    }

    case "APPLY_BACKGROUND_ACTIVITY": {
      if (action.turns.length === 0) return state
      // `updateSessionInState` materializes an empty session when the
      // conversation isn't loaded here yet — the overlay survives until the
      // tab opens and the cold detail fetch reconciles it away (its watermark
      // rule), so a completion landing on a closed tab isn't lost.
      return updateSessionInState(state, action.conversationId, (current) => {
        const indexById = new Map<string, number>()
        current.backgroundTurns.forEach((entry, i) =>
          indexById.set(entry.turn.id, i)
        )
        const next = current.backgroundTurns.slice()
        for (const turn of action.turns) {
          const entry: BackgroundOverlayEntry = {
            turn,
            watermark: action.watermark,
          }
          const existing = indexById.get(turn.id)
          if (existing === undefined) {
            indexById.set(turn.id, next.length)
            next.push(entry)
          } else {
            next[existing] = entry
          }
        }
        const bounded =
          next.length > BACKGROUND_OVERLAY_HARD_CAP
            ? next.slice(next.length - BACKGROUND_OVERLAY_HARD_CAP)
            : next
        return { ...current, backgroundTurns: bounded }
      })
    }

    case "RESOLVE_BACKGROUND_TASK": {
      // Only meaningful for an open session (a closed tab renders from the
      // disk parse, which already carries the marker). No-op otherwise — do
      // NOT materialize a session just to queue a settlement it'll never apply.
      const current = state.byConversationId.get(action.conversationId)
      if (!current) return state

      // The launch card can live in any of three places:
      //  - `optimisticTurns` (a foreground launch whose turn is mid-flight),
      //  - `localTurns` (already promoted this session), or
      //  - `detail.turns` (cold-loaded persisted history — e.g. a resumed
      //    sub-agent notifying after the tab was reopened, whose ORIGINAL card
      //    sits in detail while the newly promoted turn holds only the
      //    `SendMessage` call). We patch the in-memory `detail` copy too; the DB
      //    is never written (a later cold parse reconciles it anyway).
      const opt = applyBackgroundSettlementToTurns(
        current.optimisticTurns,
        action.settlement
      )
      const local = applyBackgroundSettlementToTurns(
        current.localTurns,
        action.settlement
      )
      const detailTurns = current.detail?.turns
      const detailRes = detailTurns
        ? applyBackgroundSettlementToTurns(detailTurns, action.settlement)
        : null

      const matched =
        opt.matched || local.matched || (detailRes?.matched ?? false)

      if (matched) {
        // Found the card — flip it (if not already showing this result) and
        // clear any stale queued copy of the same task. Both must be able to
        // fire independently: an idempotent re-settle is `matched` but not
        // `changed`, yet may still need to drop a queued entry.
        const changed =
          opt.changed || local.changed || (detailRes?.changed ?? false)
        const withoutDup = current.pendingBackgroundSettlements.filter(
          (p) => p.toolUseId !== action.settlement.toolUseId
        )
        const pendingChanged =
          withoutDup.length !== current.pendingBackgroundSettlements.length
        if (!changed && !pendingChanged) return state
        return updateSessionInState(state, action.conversationId, (s) => ({
          ...s,
          optimisticTurns: opt.turns,
          localTurns: local.turns,
          detail:
            detailRes && detailRes.changed && s.detail
              ? { ...s.detail, turns: detailRes.turns }
              : s.detail,
          pendingBackgroundSettlements: pendingChanged
            ? withoutDup
            : current.pendingBackgroundSettlements,
        }))
      }

      // Not present in any buffer yet (the #870 case: the launch tool call is
      // still in `liveMessage`, whose blocks carry no inline tool output — see
      // `LiveMessage`). Queue for `COMPLETE_TURN` to apply post-promotion.
      // De-dupe by `toolUseId` so a re-settle (resumed sub-agent) replaces the
      // queued entry instead of stacking.
      const withoutDup = current.pendingBackgroundSettlements.filter(
        (p) => p.toolUseId !== action.settlement.toolUseId
      )
      return updateSessionInState(state, action.conversationId, (s) => ({
        ...s,
        pendingBackgroundSettlements: [...withoutDup, action.settlement],
      }))
    }

    case "APPEND_OPTIMISTIC_TURN":
      // New prompt replaces activeTurnToken and cancels any pending cancel
      // reconciliation (coordinator timers are stopped by the action layer).
      return updateSessionInState(state, action.conversationId, (current) => ({
        ...current,
        optimisticTurns: [...current.optimisticTurns, action.turn],
        syncState: "awaiting_persist",
        activeTurnToken: action.turnToken,
        historyAssistantBaseline: batchStartHistoryBaseline(
          current,
          action.turn.id
        ),
        pendingCancel: null,
      }))

    case "REMOVE_OPTIMISTIC_TURN": {
      const current = state.byConversationId.get(action.conversationId)
      if (!current) return state
      const remaining = current.optimisticTurns.filter(
        (t) => t.id !== action.id
      )
      // Not found → no-op (avoid a needless re-render / identity change).
      if (remaining.length === current.optimisticTurns.length) return state
      return updateSessionInState(state, action.conversationId, (s) => ({
        ...s,
        optimisticTurns: remaining,
        // Drop back to idle once the last in-flight optimistic turn is rolled
        // back, so the `awaiting_persist` set on append doesn't linger and
        // suppress the next detail reconciliation. Concurrent optimistic turns
        // (if any) keep us awaiting_persist.
        syncState: remaining.length === 0 ? "idle" : s.syncState,
      }))
    }

    case "APPEND_VIEWER_USER_TURN": {
      const current =
        state.byConversationId.get(action.conversationId) ??
        createEmptySession(action.conversationId)
      const id = action.turn.id
      // The history boundary must be captured for this disjoint viewer batch
      // even when the prompt is DEDUPED below — a viewer attaching mid-stream
      // sees the prompt already in `detail`, so both dedup guards fire, yet the
      // reply still promotes (COMPLETE_TURN) and syncs. Without capturing here
      // the boundary stays `null`/stale and `syncTurnMetadata` folds history in.
      // `batchStartHistoryBaseline` is a no-op once the batch has turns, so a
      // dup echo mid-batch doesn't move it.
      const nextBaseline = batchStartHistoryBaseline(current, id)
      const captureOnly = (): ConversationRuntimeState =>
        nextBaseline === current.historyAssistantBaseline
          ? state
          : updateSessionInState(state, action.conversationId, (s) => ({
              ...s,
              historyAssistantBaseline: nextBaseline,
            }))
      // Cancel invalidation:
      // - Exact-id dedup (sender echo / re-delivery): keep pending cancel.
      // - Content-dedup of a *new* id (same text, distinct message id): clear
      //   pending cancel — display suppresses the optimistic copy, but this is
      //   still a new user_message that must not leave the old fence authorized.
      // - Real append: clear pending cancel.
      // Action layer always stops timers + bumps cancelGeneration for non-echo
      // paths (even when pendingCancel was already null — e.g. Stop before the
      // typed user_stop envelope), so noteUserStopTurnOwnership snapshots go
      // stale for co-controller prompts.
      // EXACT-id dedup (not a heuristic): the sender's OWN optimistic turn
      // shares this id — the UI threaded its optimistic turn id to the backend,
      // which echoed it as the `user_message` message_id — so the sender drops
      // its own echo here. Also covers an already-promoted turn (localTurns) and
      // a snapshot re-deliver. Keyed on exact id so an UNRELATED optimistic turn
      // on a co-controlling client never suppresses a DIFFERENT sender's prompt.
      //
      // `detail.turns` is checked too: while a turn is in flight the detail
      // endpoint stamps the persisted in-flight user turn with this same
      // broadcast id (see `apply_in_flight_message_id` in the backend), so the
      // synthesized copy defers to the persisted turn in its correct position.
      // This covers OpenCode and Gemini, whose transcript tail mid-stream is
      // `[.., user X, partial assistant Y]` rather than ending at the user turn —
      // the content guard below (which only matches a trailing user turn) can't
      // see X, but the backend stamp (matched by content + turn-start recency)
      // makes this id match X directly.
      //
      // Role-scoped to USER turns: every legitimate match (the sender's optimistic
      // turn, a promoted local turn, the stamped persisted prompt) is a user turn.
      // Requiring the role guards against an id collision — an unrelated ASSISTANT
      // turn that happens to share this id (only reachable via a client id that
      // slipped into another namespace) must never suppress the new prompt.
      if (isExactIdViewerUserEcho(current, id)) {
        return captureOnly()
      }
      // CONTENT dedup against persisted history. The exact-id guard above is
      // blind to the prompt once the agent has written it to its JSONL
      // transcript and it has been reloaded into `detail.turns`: the parser
      // assigns it an unrelated id there, so the synthesized turn (keyed by the
      // broadcast message_id) and the persisted turn never share an id. Without
      // this, a viewer that attaches mid-stream after the prompt was persisted
      // renders the user message twice.
      //
      // Suppress ONLY when the synthesized prompt equals the LAST persisted turn
      // AND that turn is a user turn — i.e. the transcript currently ends exactly
      // at the in-flight prompt, its reply still streaming in `liveMessage` and
      // not yet written (the normal mid-stream shape for Claude/Codex, whose
      // assistant turn is appended to the JSONL only on completion). We must NOT
      // look past a trailing assistant turn: a PREVIOUS, already-answered user
      // turn with identical text (e.g. a repeated "continue") ends with its
      // completed assistant reply, so doing so would wrongly suppress a genuinely
      // new prompt the transcript hasn't captured yet. When in doubt we keep the
      // synthesized turn visible — a transient duplicate is recoverable, a hidden
      // prompt is not. (Agents that persist a PARTIAL assistant turn mid-stream —
      // OpenCode and Gemini — end with that partial rather than the user turn, so
      // they fall through this content guard; the backend instead stamps their
      // persisted user turn with this broadcast id, handled by the exact-id guard
      // above.)
      //
      // Invariant: a trailing persisted user turn is the in-flight prompt. If a
      // prior run instead left a bare trailing user turn (crash/cancel before any
      // reply) and the user re-sends identical text, this self-corrects — the new
      // prompt is written to the transcript near-instantly, becoming the trailing
      // turn, at which point suppression of the (now redundant) synthesized copy
      // is correct. The only-suppress-on-exact-trailing-match keeps the worst case
      // a sub-second transient, never a stuck hidden prompt.
      const persistedTurns = current.detail?.turns
      const lastPersisted = persistedTurns?.[persistedTurns.length - 1]
      if (
        lastPersisted?.role === "user" &&
        userTurnContentKey(lastPersisted) === userTurnContentKey(action.turn)
      ) {
        // Same-text content dedup of a distinct message id: do not append a
        // visible optimistic copy, but still invalidate a pending cancel fence
        // so a late RECONCILE cannot clear the new prompt's live turn.
        if (
          nextBaseline === current.historyAssistantBaseline &&
          current.pendingCancel == null
        ) {
          return state
        }
        return updateSessionInState(state, action.conversationId, (s) => ({
          ...s,
          historyAssistantBaseline: nextBaseline,
          pendingCancel: null,
        }))
      }
      // Append as an optimistic turn so it flows through the EXISTING promotion
      // (COMPLETE_TURN → localTurns) and reset-on-fetch machinery, identical to
      // the owner's own user turn. Deliberately does NOT set
      // `syncState: "awaiting_persist"` — the viewer didn't send, so a later
      // detail fetch should cleanly replace the synthesized turn with persisted
      // truth (awaiting_persist would preserve it and risk a duplicate).
      //
      // New prompt from another client: clear pending cancel fence so a late
      // RECONCILE_CANCELLED_TURN cannot wipe this prompt (timers/generation are
      // always stopped in the action layer for non-exact-id paths).
      return updateSessionInState(state, action.conversationId, (s) => ({
        ...s,
        optimisticTurns: [...s.optimisticTurns, action.turn],
        historyAssistantBaseline: nextBaseline,
        pendingCancel: null,
      }))
    }

    case "SET_LIVE_MESSAGE": {
      const current = state.byConversationId.get(action.conversationId)

      // Avoid creating a ghost session when clearing liveMessage on a deleted session
      if (!current && action.liveMessage === null) return state

      const session = current ?? createEmptySession(action.conversationId)

      // Guard: prevent stale liveMessage from ACP reconnects overriding
      // persisted data. When a session has no active liveMessage and no
      // pending interaction (idle without a live turn), a SET_LIVE_MESSAGE
      // from a reconnected ACP connection carries the completed response
      // that is already in localTurns/detail.turns.
      // Accepting it would cause duplicate assistant text in the timeline.
      // Also block during cold loading (detailLoading) — the reconnect
      // liveMessage arrives before DB data, causing overlap after fetch.
      const hasExistingTurns =
        (session.detail?.turns.length ?? 0) > 0 || session.localTurns.length > 0
      if (
        !action.isLive &&
        action.liveMessage !== null &&
        session.liveMessage === null &&
        session.syncState !== "awaiting_persist" &&
        (hasExistingTurns || session.detailLoading)
      ) {
        return state
      }

      // Materialize live native activities with session agentType so Agent/Task
      // disambiguates during streaming; original tool blocks stay on liveMessage.
      let delegationActivities = session.delegationActivities
      if (action.liveMessage) {
        const next = buildStreamingTurnsForSession(
          session,
          action.liveMessage
        ).delegationActivities
        delegationActivities =
          next.length > 0 ? next : EMPTY_DELEGATION_ACTIVITIES
      } else if (!action.liveMessage && session.liveMessage) {
        // Live cleared without COMPLETE_TURN: keep last materialization.
        delegationActivities = session.delegationActivities
      }

      return updateSessionInState(state, action.conversationId, () => ({
        ...session,
        liveMessage: action.liveMessage,
        delegationActivities,
      }))
    }

    case "SET_EXTERNAL_ID": {
      const current =
        state.byConversationId.get(action.conversationId) ??
        createEmptySession(action.conversationId)
      // Rebind (external session identity change) clears pending cancel fence.
      const rebind =
        current.externalId != null && current.externalId !== action.externalId
      const nextSession: ConversationRuntimeSession = {
        ...current,
        externalId: action.externalId,
        pendingCancel: rebind ? null : current.pendingCancel,
      }
      const nextByConversationId = new Map(state.byConversationId)
      nextByConversationId.set(action.conversationId, nextSession)
      const nextExternalIndex = upsertExternalIdIndex(
        state.conversationIdByExternalId,
        current.externalId,
        action.externalId,
        action.conversationId
      )
      return {
        byConversationId: nextByConversationId,
        conversationIdByExternalId: nextExternalIndex,
      }
    }

    case "SET_DB_CONVERSATION_ID": {
      const current = state.byConversationId.get(action.conversationId)
      if (current && current.dbConversationId === action.dbConversationId) {
        return state
      }
      // Materialize like SET_EXTERNAL_ID: the binding can arrive before any
      // other action touches this session (creation resolves asynchronously).
      const base = current ?? createEmptySession(action.conversationId)
      // Replacing an existing DB binding is a backend-identity change.
      const identityReset =
        base.dbConversationId != null &&
        base.dbConversationId !== action.dbConversationId
      const nextByConversationId = new Map(state.byConversationId)
      nextByConversationId.set(action.conversationId, {
        ...base,
        dbConversationId: action.dbConversationId,
        pendingCancel: identityReset ? null : base.pendingCancel,
      })
      return { ...state, byConversationId: nextByConversationId }
    }

    case "SET_SYNC_STATE":
      return updateSessionInState(state, action.conversationId, (current) => ({
        ...current,
        syncState: action.syncState,
      }))

    case "MIGRATE_CONVERSATION": {
      if (action.fromConversationId === action.toConversationId) return state
      const from = state.byConversationId.get(action.fromConversationId)
      if (!from) return state
      const to =
        state.byConversationId.get(action.toConversationId) ??
        createEmptySession(action.toConversationId)

      const mergedLiveMessage = to.liveMessage ?? from.liveMessage

      const merged: ConversationRuntimeSession = {
        ...to,
        ...from,
        conversationId: action.toConversationId,
        detail: to.detail ?? from.detail,
        detailLoading: to.detailLoading || from.detailLoading,
        detailError: to.detailError ?? from.detailError,
        localTurns: [...from.localTurns, ...to.localTurns],
        optimisticTurns: [...from.optimisticTurns, ...to.optimisticTurns],
        liveMessage: mergedLiveMessage,
        syncState: to.syncState !== "idle" ? to.syncState : from.syncState,
        activeTurnToken: to.activeTurnToken ?? from.activeTurnToken,
        // `from` (the draft) leads `localTurns`; treat the merged buffer as
        // owner-driven if EITHER side drove its turn, so an owner's unflushed
        // reply stays protected from viewer-sync after a draft→real migration.
        lastTurnOwned: from.lastTurnOwned || to.lastTurnOwned,
        liveOwnsActiveTurn: to.liveOwnsActiveTurn || from.liveOwnsActiveTurn,
        delegationKickoffText:
          to.delegationKickoffText ?? from.delegationKickoffText,
        // `from` (the draft) leads `localTurns`, so keep its send-time
        // baseline; fall back to the target's if the draft never captured one.
        historyAssistantBaseline:
          from.historyAssistantBaseline ?? to.historyAssistantBaseline,
        // Rebind/migrate cancels any in-flight cancel reconciliation.
        pendingCancel: null,
      }

      const nextByConversationId = new Map(state.byConversationId)
      nextByConversationId.delete(action.fromConversationId)
      nextByConversationId.set(action.toConversationId, merged)

      const nextExternalIndex = new Map(state.conversationIdByExternalId)
      for (const [externalId, conversationId] of nextExternalIndex.entries()) {
        if (conversationId === action.fromConversationId) {
          nextExternalIndex.set(externalId, action.toConversationId)
        }
      }
      if (merged.externalId) {
        nextExternalIndex.set(merged.externalId, action.toConversationId)
      }

      // Drop both ids so the next select recomputes under the new conversation
      // id (keys rewrite with the new id). Never copy cache entries across.
      historicalTimelineCache.delete(action.fromConversationId)
      historicalTimelineCache.delete(action.toConversationId)

      return {
        byConversationId: nextByConversationId,
        conversationIdByExternalId: nextExternalIndex,
      }
    }

    case "PATCH_TURN_METADATA": {
      const current = state.byConversationId.get(action.conversationId)
      if (!current || current.localTurns.length === 0) return state

      const patchedTurns = [...current.localTurns]
      let changed = false
      for (const patch of action.turnPatches) {
        const turn = patchedTurns[patch.index]
        if (!turn) continue
        const newUsage =
          patch.usage === undefined ? turn.usage : (turn.usage ?? patch.usage)
        const newDuration =
          patch.duration_ms === undefined
            ? turn.duration_ms
            : (turn.duration_ms ?? patch.duration_ms)
        const newModel =
          patch.model === undefined ? turn.model : (turn.model ?? patch.model)
        const newCompletedAt =
          patch.completed_at === undefined
            ? turn.completed_at
            : (turn.completed_at ?? patch.completed_at)
        const toolMetaById =
          patch.tool_meta && patch.tool_meta.length > 0
            ? new Map(
                patch.tool_meta.map((item) => [item.tool_use_id, item.meta])
              )
            : null
        let nextBlocks = turn.blocks
        if (toolMetaById) {
          let blocksChanged = false
          nextBlocks = turn.blocks.map((block) => {
            if (
              block.type === "tool_use" &&
              block.tool_use_id &&
              toolMetaById.has(block.tool_use_id)
            ) {
              const nextMeta = toolMetaById.get(block.tool_use_id) ?? null
              if (!sameJsonValue(block.meta ?? null, nextMeta)) {
                blocksChanged = true
                return { ...block, meta: nextMeta }
              }
            }
            return block
          })
          if (!blocksChanged) nextBlocks = turn.blocks
        }
        if (
          newUsage !== turn.usage ||
          newDuration !== turn.duration_ms ||
          newModel !== turn.model ||
          newCompletedAt !== turn.completed_at ||
          nextBlocks !== turn.blocks
        ) {
          patchedTurns[patch.index] = {
            ...turn,
            blocks: nextBlocks,
            usage: newUsage,
            duration_ms: newDuration,
            model: newModel,
            completed_at: newCompletedAt,
          }
          changed = true
        }
      }

      if (!changed && !action.sessionStats) return state

      const patchedDetail =
        current.detail && action.sessionStats
          ? { ...current.detail, session_stats: action.sessionStats }
          : current.detail

      return updateSessionInState(state, action.conversationId, () => ({
        ...current,
        localTurns: changed ? patchedTurns : current.localTurns,
        detail: patchedDetail,
        sessionStats: action.sessionStats ?? current.sessionStats,
      }))
    }

    case "SET_PENDING_CLEANUP":
      return updateSessionInState(state, action.conversationId, (current) => ({
        ...current,
        pendingCleanup: action.pendingCleanup,
      }))

    case "SET_LIVE_OWNS_ACTIVE_TURN": {
      const current = state.byConversationId.get(action.conversationId)
      // No-op (don't materialize a session) when clearing an absent one with
      // no kickoff text to record.
      if (!current && !action.value && action.kickoffText == null) return state
      // `undefined` kickoffText leaves the stored value untouched.
      const nextKickoff =
        action.kickoffText !== undefined
          ? action.kickoffText
          : (current?.delegationKickoffText ?? null)
      if (
        current &&
        current.liveOwnsActiveTurn === action.value &&
        current.delegationKickoffText === nextKickoff
      ) {
        return state
      }
      return updateSessionInState(state, action.conversationId, (s) => ({
        ...s,
        liveOwnsActiveTurn: action.value,
        delegationKickoffText: nextKickoff,
      }))
    }

    case "SET_ACP_LOAD_ERROR":
      return updateSessionInState(state, action.conversationId, (current) => ({
        ...current,
        acpLoadError: action.error,
      }))

    case "RECORD_TURN_OUTCOME": {
      const current = state.byConversationId.get(action.conversationId)
      if (!current) return state
      // Idempotency by (connectionId, completionSeq) is enforced in the action
      // layer via `recordedTurnOutcomeKeys` before dispatch.
      const turns = current.localTurns
      const last = turns[turns.length - 1]
      let nextLocal: MessageTurn[]
      if (last?.role === "assistant") {
        // Attach to current-turn trailing assistant (may be empty shell).
        nextLocal = [
          ...turns.slice(0, -1),
          { ...last, outcome: action.outcome },
        ]
      } else {
        // Trailing user (or empty) → outcome-only assistant; never stamp a
        // prior non-trailing assistant (design FE case 13).
        const outcomeOnly: MessageTurn = {
          id: `cancel-outcome:${action.connectionId}:${action.completionSeq}`,
          role: "assistant",
          blocks: [],
          timestamp: new Date().toISOString(),
          outcome: action.outcome,
        }
        nextLocal = [...turns, outcomeOnly]
      }
      return updateSessionInState(state, action.conversationId, (s) => ({
        ...s,
        localTurns: nextLocal,
      }))
    }

    case "START_CANCEL_RECONCILE": {
      const current = state.byConversationId.get(action.conversationId)
      if (!current) return state
      // Idempotent when the same completion identity is already pending.
      const existing = current.pendingCancel
      if (
        existing &&
        existing.connectionId === action.key.connectionId &&
        existing.completionSeq === action.key.completionSeq &&
        existing.providerTurnId === action.key.providerTurnId
      ) {
        return state
      }
      return updateSessionInState(state, action.conversationId, (s) => ({
        ...s,
        pendingCancel: action.key,
      }))
    }

    case "RECONCILE_CANCELLED_TURN": {
      const current = state.byConversationId.get(action.conversationId)
      if (!current) return state
      const pending = current.pendingCancel
      if (
        !pending ||
        pending.connectionId !== action.key.connectionId ||
        pending.completionSeq !== action.key.completionSeq ||
        pending.cancelGeneration !== action.key.cancelGeneration ||
        pending.providerTurnId !== action.key.providerTurnId
      ) {
        return state
      }
      // Authoritative detail install + clear overlays (deterministic algorithm).
      const stamped = stampUserStopSourceOnFence(
        action.detail,
        action.key.providerTurnId
      )
      const detailWatermark = stamped.transcript_watermark ?? null
      const retainedBackground =
        detailWatermark === null
          ? current.backgroundTurns
          : current.backgroundTurns.filter((e) => e.watermark > detailWatermark)
      const agentType = stamped.summary.agent_type
      const delegationActivities = deriveActivitiesFromAssistantTurns(
        stamped.turns,
        agentType
      )
      const nextExternalId = stamped.summary.external_id ?? current.externalId
      const nextSession: ConversationRuntimeSession = {
        ...current,
        detail: stamped,
        detailLoading: false,
        detailError: null,
        delegateSyncError: null,
        externalId: nextExternalId,
        // Preserve session stats not covered by the response when absent.
        sessionStats: stamped.session_stats ?? current.sessionStats,
        localTurns: [],
        optimisticTurns: [],
        liveMessage: null,
        backgroundTurns: retainedBackground,
        // Keep unresolved background settlements, bindings, cleanup, ACP errors.
        pendingBackgroundSettlements: current.pendingBackgroundSettlements,
        pendingCleanup: current.pendingCleanup,
        acpLoadError: current.acpLoadError,
        pendingCancel: null,
        delegationActivities,
      }
      const nextByConversationId = new Map(state.byConversationId)
      nextByConversationId.set(action.conversationId, nextSession)
      const nextExternalIndex = upsertExternalIdIndex(
        state.conversationIdByExternalId,
        current.externalId,
        nextExternalId,
        action.conversationId
      )
      historicalTimelineCache.delete(action.conversationId)
      return {
        byConversationId: nextByConversationId,
        conversationIdByExternalId: nextExternalIndex,
      }
    }

    case "CLEAR_CANCEL_RECONCILE":
      return updateSessionInState(state, action.conversationId, (current) => {
        if (current.pendingCancel == null) return current
        return { ...current, pendingCancel: null }
      })

    case "REMOVE_CONVERSATION": {
      const current = state.byConversationId.get(action.conversationId)
      if (!current) return state
      const nextByConversationId = new Map(state.byConversationId)
      nextByConversationId.delete(action.conversationId)
      const nextExternalIndex = new Map(state.conversationIdByExternalId)
      if (current.externalId) {
        nextExternalIndex.delete(current.externalId)
      }
      // Drop the historical cache entry so session transcript isn't retained
      // after the conversation is removed from the store map.
      historicalTimelineCache.delete(action.conversationId)
      return {
        byConversationId: nextByConversationId,
        conversationIdByExternalId: nextExternalIndex,
      }
    }

    case "RESET":
      historicalTimelineCache.clear()
      return initialState
  }
}

// ─────────────────────────────────────────────────────────────────────────
// Store wiring
// ─────────────────────────────────────────────────────────────────────────

export interface RuntimeActions {
  fetchDetail: (conversationId: number) => void
  refetchDetail: (
    conversationId: number,
    options?: { preserveLive?: boolean }
  ) => void
  /**
   * Manual Reload: clear any pending cancel fence, then perform an
   * authoritative detail load (may resolve negative runtime ids via
   * `dbConversationId`).
   */
  reloadDetail: (
    conversationId: number,
    options: { reason: "manual_reload" }
  ) => void
  /**
   * Attach interrupted `TurnOutcome` to the current-turn assistant or an
   * outcome-only turn. Idempotent by `(connectionId, completionSeq)`.
   */
  recordTurnOutcome: (params: {
    conversationId: number
    connectionId: string
    completionSeq: number
    outcome: TurnOutcome
  }) => void
  /**
   * Start the abort-fenced cancel reconciliation coordinator (raw detail
   * reads only; applies solely via `RECONCILE_CANCELLED_TURN`).
   */
  startCancelReconcile: (params: {
    conversationId: number
    connectionId: string
    completionSeq: number
    providerTurnId: string
    activeTurnToken?: string | null
  }) => void
  /** Clear pending cancel key + stop coordinator timers (lifecycle table). */
  clearCancelReconcile: (conversationId: number) => void
  /**
   * Poll a passively-viewed conversation's persisted detail into sync after its
   * turn completed on another client. No-op unless the session is open and this
   * client is a pure viewer of it (never touches an owner's in-memory reply).
   */
  syncViewerDetail: (conversationId: number) => void
  /**
   * Bounded poll that converges a delegated child's terminal transcript into
   * persisted detail without dropping live/local content mid-window.
   */
  syncDelegateTerminalDetail: (conversationId: number) => void
  syncTurnMetadata: (
    dbConversationId: number,
    runtimeConversationId?: number
  ) => () => void
  completeTurn: (
    conversationId: number,
    liveMessage?: LiveMessage | null
  ) => void
  appendOptimisticTurn: (
    conversationId: number,
    turn: MessageTurn,
    turnToken: string
  ) => void
  removeOptimisticTurn: (conversationId: number, id: string) => void
  appendViewerUserTurn: (conversationId: number, turn: MessageTurn) => void
  applyBackgroundActivity: (
    conversationId: number,
    turns: MessageTurn[],
    watermark: number
  ) => void
  resolveBackgroundTask: (
    conversationId: number,
    settlement: PendingBackgroundSettlement
  ) => void
  setLiveMessage: (
    conversationId: number,
    liveMessage: LiveMessage | null,
    isLive?: boolean,
    /** Optional ACP delivery IDs for streaming-perf attribution (P0). */
    deliveryIds?: readonly number[]
  ) => void
  setExternalId: (conversationId: number, externalId: string | null) => void
  setDbConversationId: (
    conversationId: number,
    dbConversationId: number | null
  ) => void
  setSyncState: (
    conversationId: number,
    syncState: ConversationSyncState
  ) => void
  migrateConversation: (
    fromConversationId: number,
    toConversationId: number
  ) => void
  setPendingCleanup: (conversationId: number, pendingCleanup: boolean) => void
  setAcpLoadError: (conversationId: number, error: string | null) => void
  setLiveOwnsActiveTurn: (
    conversationId: number,
    value: boolean,
    kickoffText?: string | null
  ) => void
  removeConversation: (conversationId: number) => void
  reset: () => void
}

interface ConversationRuntimeStore extends ConversationRuntimeState {
  actions: RuntimeActions
}

/** Shape returned by the `useConversationRuntime()` compatibility shim. */
export interface ConversationRuntimeContextValue extends RuntimeActions {
  getSession: (conversationId: number) => ConversationRuntimeSession | null
  getConversationIdByExternalId: (externalId: string) => number | null
  getTimelineTurns: (conversationId: number) => ConversationTimelineTurn[]
}

// Per-conversation fetch-generation counter. Each fetchDetail / refetchDetail /
// removeConversation bumps the counter for that conversationId; an outstanding
// fetch captures the value it was issued with and refuses to dispatch its
// success/error if the counter has moved on. Closes the stale-response-overwrite
// and resurrection-after-remove races. Cells are kept indefinitely (small int
// per conversation); a cleanup sweep isn't needed for the expected cardinality.
const fetchGeneration = new Map<number, number>()

function bumpFetchGeneration(conversationId: number): number {
  const next = (fetchGeneration.get(conversationId) ?? 0) + 1
  fetchGeneration.set(conversationId, next)
  return next
}

function isLatestGeneration(
  conversationId: number,
  generation: number
): boolean {
  return fetchGeneration.get(conversationId) === generation
}

// ─── User-stop cancel reconciliation ─────────────────────────────────────
// Dedicated generation (not fetchGeneration): bumps on new prompt, remove,
// rebind, backend identity reset, Manual Reload start, coordinator success,
// and coordinator terminal failure (retry exhaustion).
const cancelGenerationById = new Map<number, number>()
// Active coordinator cancel fns (stop timers + in-flight attempt).
const cancelReconcileCancels = new Map<number, () => void>()
// Idempotency for RECORD_TURN_OUTCOME: conversationId → `connectionId\0seq`.
const recordedTurnOutcomeKeys = new Map<number, string>()
/**
 * Ownership fence for dual-path cancel envelopes, snapshotted when the user
 * invokes Cancel (before `turn_complete` may arrive late).
 *
 * Uses **monotonic `cancelGeneration`** (not only nullable `activeTurnToken`):
 * a next prompt bumps generation, so a late envelope after that prompt — even
 * after the next turn has completed and cleared `activeTurnToken` — is still
 * rejected and cannot stamp or reconcile under the newer transcript.
 */
interface UserStopOwnership {
  activeTurnToken: string | null
  cancelGeneration: number
}

const userStopOwnershipById = new Map<number, UserStopOwnership>()

function getCancelGeneration(conversationId: number): number {
  return cancelGenerationById.get(conversationId) ?? 0
}

function bumpCancelGeneration(conversationId: number): number {
  const next = getCancelGeneration(conversationId) + 1
  cancelGenerationById.set(conversationId, next)
  return next
}

/**
 * Resolve the runtime session map key for cancel ownership.
 * Draft-originated sessions stay under a negative `effectiveConversationId`
 * even after a positive `dbConversationId` is bound; prefer an existing map
 * entry, then a session whose `dbConversationId` matches a positive id.
 */
export function resolveRuntimeConversationIdForOwnership(
  conversationId: number
): number | null {
  const state = useConversationRuntimeStore.getState()
  if (state.byConversationId.has(conversationId)) return conversationId
  for (const [id, session] of state.byConversationId) {
    if (session.dbConversationId === conversationId) return id
  }
  return null
}

/**
 * Snapshot cancel-time ownership (token + cancelGeneration) for the runtime
 * session. Accepts either the runtime map key or a positive DB id that is only
 * bound as `dbConversationId` on a virtual/negative session.
 * Called from the ACP cancel path before the backend emits `turn_complete`.
 */
export function noteUserStopTurnOwnership(conversationId: number): void {
  const runtimeId = resolveRuntimeConversationIdForOwnership(conversationId)
  if (runtimeId == null) return
  const session = useConversationRuntimeStore
    .getState()
    .byConversationId.get(runtimeId)
  if (!session) return
  userStopOwnershipById.set(runtimeId, {
    activeTurnToken: session.activeTurnToken,
    cancelGeneration: getCancelGeneration(runtimeId),
  })
}

/** @internal Test helper — ownership record for a conversation (runtime key). */
export function __getUserStopOwnershipForTests(
  conversationId: number
): UserStopOwnership | undefined {
  if (process.env.NODE_ENV !== "test") return undefined
  const runtimeId =
    resolveRuntimeConversationIdForOwnership(conversationId) ?? conversationId
  return userStopOwnershipById.get(runtimeId)
}

/** @internal Test helper — current cancelGeneration for a conversation. */
export function __getCancelGenerationForTests(conversationId: number): number {
  if (process.env.NODE_ENV !== "test") return 0
  const runtimeId =
    resolveRuntimeConversationIdForOwnership(conversationId) ?? conversationId
  return getCancelGeneration(runtimeId)
}

/**
 * Whether a late user_stop envelope is stale: cancelGeneration has advanced
 * past the value snapshotted at Stop (next prompt / lifecycle invalidation).
 */
export function isStaleUserStopEnvelope(conversationId: number): boolean {
  const runtimeId =
    resolveRuntimeConversationIdForOwnership(conversationId) ?? conversationId
  const owned = userStopOwnershipById.get(runtimeId)
  if (!owned) return false
  if (!useConversationRuntimeStore.getState().byConversationId.has(runtimeId)) {
    return true
  }
  return getCancelGeneration(runtimeId) !== owned.cancelGeneration
}

/** Token to fence cancel reconcile / outcome for the cancelled completion. */
export function getUserStopFenceToken(
  conversationId: number
): string | null | undefined {
  const runtimeId =
    resolveRuntimeConversationIdForOwnership(conversationId) ?? conversationId
  const owned = userStopOwnershipById.get(runtimeId)
  if (owned) return owned.activeTurnToken
  const session = useConversationRuntimeStore
    .getState()
    .byConversationId.get(runtimeId)
  return session?.activeTurnToken
}

function stopCancelReconcileTimers(conversationId: number): void {
  const cancel = cancelReconcileCancels.get(conversationId)
  if (cancel) cancel()
}

function detailHasMatchingCancelFence(
  detail: DbConversationDetail,
  providerTurnId: string
): boolean {
  return detail.turns.some(
    (t) =>
      t.outcome?.status === "interrupted" &&
      t.outcome.stop_reason === "cancelled" &&
      t.outcome.provider_turn_id != null &&
      t.outcome.provider_turn_id === providerTurnId
  )
}

/** Carry live `source = "user_stop"` onto the matched interrupted outcome. */
function stampUserStopSourceOnFence(
  detail: DbConversationDetail,
  providerTurnId: string
): DbConversationDetail {
  let changed = false
  const turns = detail.turns.map((t) => {
    if (
      t.outcome?.status === "interrupted" &&
      t.outcome.provider_turn_id === providerTurnId
    ) {
      if (t.outcome.source === "user_stop") return t
      changed = true
      return {
        ...t,
        outcome: {
          ...t.outcome,
          source: "user_stop" as const,
        },
      }
    }
    return t
  })
  return changed ? { ...detail, turns } : detail
}

function resolvePersistedConversationId(
  session: ConversationRuntimeSession | undefined,
  runtimeConversationId: number
): number {
  return session?.dbConversationId ?? runtimeConversationId
}

function sessionHasPendingCancel(
  session: ConversationRuntimeSession | undefined | null
): boolean {
  return session?.pendingCancel != null
}

/**
 * Exact-id sender-echo for APPEND_VIEWER_USER_TURN: the sender's optimistic
 * turn, a promoted local user turn, or a detail turn stamped with the same
 * broadcast id. These are re-deliveries of an already-known prompt — not a
 * new co-controller message — and must keep cancel timers/ownership.
 */
function isExactIdViewerUserEcho(
  session: ConversationRuntimeSession | undefined,
  turnId: string
): boolean {
  if (!session) return false
  return (
    session.optimisticTurns.some((t) => t.id === turnId && t.role === "user") ||
    session.localTurns.some((t) => t.id === turnId && t.role === "user") ||
    (session.detail?.turns.some((t) => t.id === turnId && t.role === "user") ??
      false)
  )
}

// ─── Cross-client viewer detail sync ─────────────────────────────────────
// A conversation whose turn completes on ANOTHER client (this client is only
// VIEWING it) has no live promotion path here: the panel's promotion is edge-
// triggered on the connection's `prompting → connected` transition, which a
// viewer that missed the (short) live stream never observes, and the global
// `conversation://changed` side-channel only patches the sidebar list — not the
// open conversation's detail. So the viewer keeps rendering its stale detail
// (the prompt, no reply). The fix polls the persisted transcript on that nudge.
//
// The catch (verified against the backend): every turn-end signal
// (`conversation://changed` Status, `turn_complete`) fires off the ACP wire
// stop-reason, which RACES the agent CLI flushing its transcript JSONL. Detail
// is a live parse of whatever bytes are on disk, so a single refetch can still
// return the pre-reply transcript. We therefore poll a bounded number of times,
// backing off, and stop as soon as the transcript's last turn is no longer a
// trailing USER turn (Claude/Codex append the assistant reply to the JSONL only
// on completion, so a trailing user turn means the reply is still mid-flush).
const VIEWER_DETAIL_SYNC_DELAYS_MS = [0, 300, 700, 1500, 2500] as const

// Active viewer-sync polls, keyed by conversationId, so a fresh nudge supersedes
// an in-flight poll (never stacks) and `removeConversation` / store reset can
// cancel a poll whose tab has closed.
const viewerDetailSyncCancels = new Map<number, () => void>()

// Separate cancel map so viewer and delegate terminal policies cannot cancel
// each other's ownership accidentally.
const delegateTerminalSyncCancels = new Map<number, () => void>()

// Terminal triggers can fire before the first detail fetch lands (prompting→
// connected edge, access leave task_running, reconnect). While detail is null,
// mark the conversation so any in-flight / seed FETCH_DETAIL_SUCCESS preserves
// live buffers and re-enters terminal convergence after seed.
const pendingDelegateTerminalSync = new Set<number>()

// Assigned once the runtime actions close over `syncDelegateTerminalDetail`.
// Used by detail-fetch success paths that resolve before that function's
// declaration (and by the seed path re-entry).
let resumeDelegateTerminalSync: ((conversationId: number) => void) | null = null

/** Terminal child task statuses that own transcript convergence. */
const TERMINAL_DELEGATE_TASK_STATES = new Set([
  "completed",
  "failed",
  "canceled",
])

function isTerminalDelegateTaskStatus(
  status: string | null | undefined
): boolean {
  return status != null && TERMINAL_DELEGATE_TASK_STATES.has(status)
}

function cancelViewerDetailSync(conversationId: number): void {
  const cancel = viewerDetailSyncCancels.get(conversationId)
  if (cancel) cancel()
}

function cancelAllDetailSyncs(): void {
  for (const cancel of viewerDetailSyncCancels.values()) cancel()
  viewerDetailSyncCancels.clear()
  for (const cancel of delegateTerminalSyncCancels.values()) cancel()
  delegateTerminalSyncCancels.clear()
  pendingDelegateTerminalSync.clear()
  for (const cancel of cancelReconcileCancels.values()) cancel()
  cancelReconcileCancels.clear()
}

function schedulePendingDelegateTerminalSync(conversationId: number): void {
  if (!pendingDelegateTerminalSync.has(conversationId)) return
  queueMicrotask(() => {
    if (!pendingDelegateTerminalSync.has(conversationId)) return
    resumeDelegateTerminalSync?.(conversationId)
  })
}

/**
 * After a successful plain detail fetch: resume a pending terminal sync, or
 * auto-start one when the loaded detail is already a terminal delegate.
 *
 * Workspace upserts can call `syncDelegateTerminalDetail` before any runtime
 * session exists — that signal is dropped. Opening the child later must still
 * converge, so first detail success for kind=delegate + terminal task status
 * starts the poll when nothing is already in flight.
 */
function afterDetailFetchSuccess(
  conversationId: number,
  detail: DbConversationDetail
): void {
  if (pendingDelegateTerminalSync.has(conversationId)) {
    schedulePendingDelegateTerminalSync(conversationId)
    return
  }
  if (
    detail.summary.kind !== "delegate" ||
    !isTerminalDelegateTaskStatus(detail.summary.delegation_task_status)
  ) {
    return
  }
  // Do not restart an active terminal poll (reconnect / explicit trigger).
  if (delegateTerminalSyncCancels.has(conversationId)) return
  queueMicrotask(() => {
    if (delegateTerminalSyncCancels.has(conversationId)) return
    resumeDelegateTerminalSync?.(conversationId)
  })
}

const DELEGATE_TERMINAL_SYNC_FAILED =
  "Delegated transcript did not converge before the retry window ended"

// Resolve the RUNTIME-session key for a `conversation://changed` nudge, which
// carries the positive DB id. A tab opened from a draft keeps its virtual
// (negative) runtime key for its whole life while storing the positive id in
// `dbConversationId` (see `conversation-detail-panel.tsx`), so a direct lookup
// by the nudged id misses it — fall back to a scan over the (few) open sessions.
// A positive-keyed session is preferred when both exist. Returns null when no
// open session matches, so a nudge for an unopened conversation cheaply no-ops.
function resolveViewerRuntimeId(
  byConversationId: Map<number, ConversationRuntimeSession>,
  conversationId: number
): number | null {
  if (byConversationId.has(conversationId)) return conversationId
  for (const [key, session] of byConversationId) {
    if (session.dbConversationId === conversationId) return key
  }
  return null
}

// A "pure viewer" holds none of the in-memory copies of a reply that a transcript
// refetch could race and clobber: no in-flight prompt (`awaiting_persist`), no
// live stream (`liveMessage`), and no just-PROMOTED reply that persistence may
// still lag. Only such a session may be refetched from the transcript.
//
// `localTurns` alone is NOT disqualifying: a viewer that streamed an EARLIER turn
// promotes it into `localTurns` too, and that reply is already persisted (it
// completed on the owner before this client observed the edge), so folding it
// from disk is safe. Excluding every `localTurns` would permanently strand such
// a viewer after its first captured turn — reproducing the very bug this sync
// fixes. Only a PROMOTED reply that may still be mid-flush (`completeTurn`'s ~8ms
// write race) must be protected, i.e. `localTurns.length > 0` AND either:
//   - `lastTurnOwned` — this client DROVE the promoted turn (an owner send); its
//     reply lives only in `localTurns` until the transcript catches up; or
//   - `liveOwnsActiveTurn` — a delegation child conversation tab adopted its
//     reply from the wire ahead of persistence (and deliberately does NOT
//     refetch) and owns its promotion/dedup path.
// Both are gated on `localTurns.length > 0`: the pre-promotion streaming phase is
// already covered by `liveMessage`, and a MARKER-ONLY delegation child (the no-
// child-connection fallback that never streams or promotes) has nothing to guard,
// so it must stay eligible to sync on a later completion nudge. The synthesized
// viewer user turn lives in `optimisticTurns` WITHOUT `awaiting_persist`, so it
// never blocks a detail load from replacing it.
//
// Known limitation: a client that DROVE a turn and then passively MISSES every
// later turn (no live stream at all — a co-controlling client drove them) keeps
// `lastTurnOwned` set and stays excluded until a settled fetch (tab switch /
// reopen) clears its `localTurns`. This is deliberate: the alternative — admitting
// a session whose `localTurns` may hold an unflushed reply — reintroduces the
// hardware-evidenced content-drop `completeTurn` documents. The failure is safe
// (stale, never wrong) and self-heals on any refetch or the next observed turn
// (whose promotion re-stamps `lastTurnOwned` false). The common viewer — one that
// never drove a turn — is unaffected.
function isPureViewerSession(session: ConversationRuntimeSession): boolean {
  return (
    session.syncState !== "awaiting_persist" &&
    session.liveMessage === null &&
    !(
      session.localTurns.length > 0 &&
      (session.lastTurnOwned || session.liveOwnsActiveTurn)
    )
  )
}

interface DelegateTerminalSyncAnchor {
  userId: string
  persistedUserId: string | null
  contentKey: string
  minPersistedIndex: number
}

function captureDelegateTerminalSyncAnchor(
  state: ConversationRuntimeState,
  conversationId: number
): DelegateTerminalSyncAnchor | null {
  const session = state.byConversationId.get(conversationId)
  if (!session) return null
  const timeline = computeTimeline(state, conversationId)
  let visibleUser: MessageTurn | null = null
  for (let index = timeline.length - 1; index >= 0; index -= 1) {
    if (timeline[index].turn.role === "user") {
      visibleUser = timeline[index].turn
      break
    }
  }
  if (!visibleUser) return null

  const persisted = session.detail?.turns ?? []
  const contentKey = userTurnContentKey(visibleUser)
  let persistedIndex = persisted.findIndex(
    (turn) => turn.role === "user" && turn.id === visibleUser.id
  )
  if (persistedIndex < 0 && session.detail?.in_flight_user_turn_id != null) {
    const inFlightIndex = persisted.findIndex(
      (turn) =>
        turn.role === "user" &&
        turn.id === session.detail?.in_flight_user_turn_id
    )
    if (
      inFlightIndex >= 0 &&
      userTurnContentKey(persisted[inFlightIndex]) === contentKey
    ) {
      persistedIndex = inFlightIndex
    }
  }
  if (persistedIndex < 0) {
    let trailingUserIndex = -1
    for (let index = persisted.length - 1; index >= 0; index -= 1) {
      if (persisted[index].role === "user") {
        trailingUserIndex = index
        break
      }
    }
    const hasAssistantAfter = persisted
      .slice(trailingUserIndex + 1)
      .some((turn) => turn.role === "assistant")
    if (
      trailingUserIndex >= 0 &&
      !hasAssistantAfter &&
      userTurnContentKey(persisted[trailingUserIndex]) === contentKey
    ) {
      persistedIndex = trailingUserIndex
    }
  }
  return {
    userId: visibleUser.id,
    persistedUserId: persistedIndex >= 0 ? persisted[persistedIndex].id : null,
    contentKey,
    minPersistedIndex: persistedIndex >= 0 ? persistedIndex : persisted.length,
  }
}

function delegateTerminalDetailConverged(
  detail: DbConversationDetail,
  anchor: DelegateTerminalSyncAnchor | null
): boolean {
  if (!anchor || detail.in_flight_user_turn_id != null) return false
  const start = Math.min(anchor.minPersistedIndex, detail.turns.length)
  let userIndex = detail.turns.findIndex(
    (turn, index) =>
      index >= start &&
      turn.role === "user" &&
      (turn.id === anchor.userId || turn.id === anchor.persistedUserId)
  )
  if (userIndex < 0) {
    userIndex = detail.turns.findIndex(
      (turn, index) =>
        index >= start &&
        turn.role === "user" &&
        userTurnContentKey(turn) === anchor.contentKey
    )
  }
  return (
    userIndex >= 0 &&
    detail.turns.slice(userIndex + 1).some((turn) => turn.role === "assistant")
  )
}

/**
 * Stable two-list merge by turn timestamp (both inputs are already in
 * chronological order within themselves). Ties keep the LEFT list's entry
 * first. Timestamps are parsed (not string-compared) because the parser's
 * RFC3339 output carries variable sub-second precision.
 */
function mergeTimelineByTimestamp(
  a: ConversationTimelineTurn[],
  b: ConversationTimelineTurn[]
): ConversationTimelineTurn[] {
  if (a.length === 0) return b
  if (b.length === 0) return a
  const at = a.map((e) => Date.parse(e.turn.timestamp) || 0)
  const bt = b.map((e) => Date.parse(e.turn.timestamp) || 0)
  const merged: ConversationTimelineTurn[] = []
  let i = 0
  let j = 0
  while (i < a.length && j < b.length) {
    if (at[i] <= bt[j]) {
      merged.push(a[i])
      i += 1
    } else {
      merged.push(b[j])
      j += 1
    }
  }
  while (i < a.length) {
    merged.push(a[i])
    i += 1
  }
  while (j < b.length) {
    merged.push(b[j])
    j += 1
  }
  return merged
}

/**
 * Historical (non-streaming) timeline: phases 1–3 only. Cached per conversation
 * with a key that excludes live content so streaming token appends keep the
 * same historical array and entry references.
 */
function computeHistoricalTimeline(
  state: ConversationRuntimeState,
  conversationId: number
): ConversationTimelineTurn[] {
  const session = state.byConversationId.get(conversationId)
  if (!session) return EMPTY_TIMELINE

  const key = buildHistoricalKey(session)
  const cached = historicalTimelineCache.get(conversationId)
  if (cached && sameHistoricalKey(cached.key, key)) {
    return cached.value
  }

  // Phase 1: DB historical turns.
  // When liveOwnsActiveTurn is set (sub-agent dialog), the live/local reply
  // is authoritative for the child's current (only) reply. Strip any
  // persisted assistant turns while there's a live or just-promoted local
  // reply in this session — only the kickoff prefix (everything before the
  // first assistant turn) is shown from the DB. This eliminates the
  // partial-plus-live duplicate for all timing scenarios, including a
  // connection-id-null open where we can't read the live store during fetch.
  //
  // Delegation children are SINGLE-REPLY (one-shot): stripping from the
  // first assistant turn onward removes exactly the persisted copy of that
  // one reply. (A hypothetical multi-turn child would have earlier replies
  // hidden during the live/grace window — not a case the viewer supports.)
  //
  // Identity-only live fields (`liveMessageId` / `liveStartedAt`) gate the
  // same branches that previously checked `session.liveMessage !== null`, so
  // content-only live updates do not recompute this path.
  const liveMessageId = key.liveMessageId
  const liveStartedAt = key.liveStartedAt
  const rawPersistedTurns = session.detail?.turns ?? []
  const hasLiveOrLocalReply =
    session.liveOwnsActiveTurn &&
    (liveMessageId !== null || session.localTurns.length > 0)
  const firstAssistantIdx = hasLiveOrLocalReply
    ? rawPersistedTurns.findIndex((t) => t.role === "assistant")
    : -1
  const persistedTurns =
    hasLiveOrLocalReply && firstAssistantIdx !== -1
      ? rawPersistedTurns.slice(0, firstAssistantIdx)
      : rawPersistedTurns

  // Suppress the persisted PARTIAL in-flight reply for a non-delegation
  // cross-client viewer. While a reply is streaming, some agents (OpenCode,
  // Gemini) persist a partial assistant turn for it under a parser id; loaded
  // into `detail` it sits beside the live reply (a separate assistant turn
  // under a `live-…` id), and `mergeConsecutiveAssistantTurns` concatenates
  // the two — so the already-persisted head (e.g. the first reasoning block)
  // renders twice. Hide that persisted partial, but ONLY while `liveMessage`
  // is in hand: the live stream carries the full reply (the attach snapshot is
  // built atomically and includes it), so this only ever hides from render
  // what the live stream is concurrently showing — never dropping a reply we
  // can't re-show. The moment the turn ends, `liveMessage` clears and the
  // persisted copy (now complete) renders normally; the brief promote→refetch
  // grace window can show a transient visible duplicate, never a hidden turn.
  //
  // The in-flight prompt is identified authoritatively by the backend, which
  // reports the id of the persisted user turn it stamped as the in-flight one
  // (`detail.in_flight_user_turn_id`). This is robust where a frontend anchor
  // is not: the viewer's synthesized prompt may be suppressed (the persisted
  // copy already carries the broadcast id), and `liveMessage.startedAt` is the
  // client clock on the streaming path — neither can locate the prompt across
  // machines. When the new prompt isn't persisted yet the backend reports no
  // id, so an earlier completed round's reply is never mistaken for a partial.
  const inFlightPromptId = session.detail?.in_flight_user_turn_id ?? null
  const inFlightPromptIdx =
    !hasLiveOrLocalReply && liveMessageId !== null && inFlightPromptId !== null
      ? persistedTurns.findIndex(
          (t) => t.role === "user" && t.id === inFlightPromptId
        )
      : -1
  const visiblePersistedTurns =
    inFlightPromptIdx === -1
      ? persistedTurns
      : persistedTurns.filter(
          (t, i) => i <= inFlightPromptIdx || t.role !== "assistant"
        )

  const persisted: ConversationTimelineTurn[] = visiblePersistedTurns.map(
    (turn, index) => ({
      key: `persisted-${conversationId}-${turn.id}-${index}`,
      turn,
      phase: "persisted" as const,
    })
  )

  // Synthetic delegation kickoff. The child agent CLI writes its JSONL
  // transcript asynchronously, so the persisted detail can lag the live
  // stream by up to seconds — during which `persistedTurns` carries no user
  // turn and the dialog would show the streaming reply with no kickoff above
  // it. When this is a delegation-child viewer (`liveOwnsActiveTurn`) and no
  // persisted user turn has surfaced yet, synthesize the kickoff from the
  // known parent task text so it shows immediately. The moment the real
  // persisted user turn lands, this condition turns off and the authentic
  // turn is used instead — no duplicate, no cleanup needed.
  if (
    session.liveOwnsActiveTurn &&
    session.delegationKickoffText &&
    !persistedTurns.some((t) => t.role === "user")
  ) {
    persisted.unshift({
      key: `kickoff-${conversationId}`,
      turn: {
        id: `kickoff-${conversationId}`,
        role: "user",
        blocks: [{ type: "text", text: session.delegationKickoffText }],
        // Best-effort timestamp: the persisted summary (once loaded) or the
        // live reply's start; falls back to "" only in the brief window
        // before either exists. Consumers in the render path tolerate "";
        // the fallbacks keep date formatters off an empty string in the
        // common case.
        timestamp:
          session.detail?.summary.created_at ??
          (liveStartedAt !== null ? new Date(liveStartedAt).toISOString() : ""),
      },
      phase: "persisted",
    })
  }

  // Phase 2: Locally completed turns (promoted optimistic + completed streaming)
  const local: ConversationTimelineTurn[] = session.localTurns.map(
    (turn, index) => ({
      key: `local-${conversationId}-${turn.id}-${index}`,
      turn,
      phase: "persisted",
    })
  )

  // Phase 2.5: background overlay turns — out-of-turn activity (async task
  // completions, the agent's continued work after them, cron//loop turns)
  // parsed from the agent's own transcript by the backend watcher. They are
  // already-persisted facts shown ahead of the next detail refetch, which
  // retires them via the watermark rule (see FETCH_DETAIL_SUCCESS) — so a
  // persisted copy and an overlay copy never coexist here. Interleaved with
  // localTurns by timestamp so a foreground exchange completed BETWEEN
  // background turns keeps wall-clock order.
  const background: ConversationTimelineTurn[] = session.backgroundTurns.map(
    (entry) => ({
      key: `background-${conversationId}-${entry.turn.id}`,
      turn: entry.turn,
      phase: "persisted" as const,
    })
  )
  const localAndBackground =
    background.length === 0
      ? local
      : mergeTimelineByTimestamp(local, background)

  // Phase 3: Optimistic turns (pending user messages)
  const optimistic: ConversationTimelineTurn[] = session.optimisticTurns.map(
    (turn, index) => ({
      key: `optimistic-${conversationId}-${turn.id}-${index}`,
      turn,
      phase: "optimistic",
    })
  )

  // Invariant: the historical timeline never contains two turns with the same
  // id+role. Streaming-phase dedup against historical runs in
  // `appendCanonicalStreamingTurns` so this cache stays free of live content.
  const result = [...persisted, ...localAndBackground, ...optimistic]
  const deduped = dedupeTimelineByRoleAwareId(result)

  historicalTimelineCache.set(conversationId, { key, value: deduped })
  return deduped
}

/**
 * Append canonical live streaming turns onto a historical timeline without
 * mutating the cached historical array. Dedup is role-aware so a promoted
 * local snapshot yields to the still-streaming copy of the same live id.
 */
function appendCanonicalStreamingTurns(
  historical: ConversationTimelineTurn[],
  conversationId: number,
  liveMessage: LiveMessage,
  agentType?: AgentType | null
): ConversationTimelineTurn[] {
  const built = buildStreamingTurnsFromLiveMessage(
    conversationId,
    liveMessage,
    {
      agentType: agentType ?? null,
    }
  )
  const result = historical.slice()
  for (const [index, turn] of built.turns.entries()) {
    result.push({
      key: `streaming-${conversationId}-${liveMessage.id}-${index}`,
      turn,
      phase: "streaming",
      inProgressToolCallIds: built.inProgressToolCallIds,
    })
  }
  return dedupeTimelineByRoleAwareId(result)
}

/**
 * Full render timeline: stable historical rows + (when live) canonical
 * streaming turns. Streaming appends produce a new array each call; historical
 * entries remain reference-stable across content-only live updates.
 */
function computeTimeline(
  state: ConversationRuntimeState,
  conversationId: number
): ConversationTimelineTurn[] {
  const historical = computeHistoricalTimeline(state, conversationId)
  const session = state.byConversationId.get(conversationId)
  if (!session?.liveMessage) return historical
  return appendCanonicalStreamingTurns(
    historical,
    conversationId,
    session.liveMessage,
    resolveSessionAgentType(session)
  )
}

export const useConversationRuntimeStore = create<ConversationRuntimeStore>()((
  set,
  get
) => {
  const dispatch = (action: Action): void => {
    set((state) => reducer(state, action))
  }

  const fetchDetail = (conversationId: number): void => {
    const session = get().byConversationId.get(conversationId)
    if (session?.detail || session?.detailLoading) return
    if (sessionHasPendingCancel(session)) return

    // Skip fetch if session has active data (ongoing conversation)
    if (
      session &&
      (session.optimisticTurns.length > 0 ||
        session.liveMessage !== null ||
        session.localTurns.length > 0)
    ) {
      return
    }

    const generation = bumpFetchGeneration(conversationId)
    dispatch({ type: "FETCH_DETAIL_START", conversationId })
    getFolderConversation(conversationId)
      .then((detail) => {
        if (!isLatestGeneration(conversationId, generation)) return
        // Exclusive path: a cancel fence may start while this cold fetch is
        // in flight — never commit unfenced detail over pending cancel.
        if (
          sessionHasPendingCancel(get().byConversationId.get(conversationId))
        ) {
          endDetailLoadingWithoutCommit(conversationId)
          return
        }
        // Terminal sync may have been requested while detail was still empty:
        // force preserveLive so the first commit cannot wipe live buffers.
        const preserveLive = pendingDelegateTerminalSync.has(conversationId)
        dispatch({
          type: "FETCH_DETAIL_SUCCESS",
          conversationId,
          detail,
          preserveLive,
        })
        afterDetailFetchSuccess(conversationId, detail)
      })
      .catch((error: unknown) => {
        if (!isLatestGeneration(conversationId, generation)) return
        if (
          sessionHasPendingCancel(get().byConversationId.get(conversationId))
        ) {
          endDetailLoadingWithoutCommit(conversationId)
          return
        }
        const message = toErrorMessage(error)
        // Pending terminal sync was waiting on this seed commit — clear the
        // stranded flag, surface a visible failure, and leave recovery free
        // to re-queue (explicit trigger / reopen / later detail load).
        if (pendingDelegateTerminalSync.has(conversationId)) {
          pendingDelegateTerminalSync.delete(conversationId)
          dispatch({
            type: "SET_DELEGATE_SYNC_ERROR",
            conversationId,
            error: message,
          })
        }
        dispatch({
          type: "FETCH_DETAIL_ERROR",
          conversationId,
          error: message,
        })
      })
  }

  const refetchDetail = (
    conversationId: number,
    options?: { preserveLive?: boolean }
  ): void => {
    // The session key is not always a fetchable DB id: a conversation started
    // as a new-chat draft keeps its virtual (negative) key for the tab's whole
    // life. Fetch with the bound DB row id (see `dbConversationId`) and store
    // the result back under the runtime key. Without this, a settle-driven
    // refetch (background_activity → refetchDetail(runtimeKey)) asks the
    // backend for a nonexistent conversation, errors silently, and the stale
    // live buffers — e.g. an async sub-agent card frozen on its launch ack —
    // never flip to their persisted terminal state.
    const session = get().byConversationId.get(conversationId)
    // Exclusive destructive path: while a cancel fence is pending, automatic
    // refetch must not commit (Manual Reload uses `reloadDetail` which clears
    // the key first). preserveLive-only callers still must not install
    // unfenced detail over a pending cancel.
    if (sessionHasPendingCancel(session)) {
      return
    }
    const fetchId = session?.dbConversationId ?? conversationId
    const generation = bumpFetchGeneration(conversationId)
    dispatch({ type: "FETCH_DETAIL_START", conversationId })
    getFolderConversation(fetchId)
      .then((detail) => {
        if (!isLatestGeneration(conversationId, generation)) return
        // Re-check fence: a cancel may have started while the fetch was in flight.
        if (
          sessionHasPendingCancel(get().byConversationId.get(conversationId))
        ) {
          endDetailLoadingWithoutCommit(conversationId)
          return
        }
        const preserveLive =
          options?.preserveLive === true ||
          pendingDelegateTerminalSync.has(conversationId)
        dispatch({
          type: "FETCH_DETAIL_SUCCESS",
          conversationId,
          detail,
          preserveLive,
        })
        afterDetailFetchSuccess(conversationId, detail)
      })
      .catch((error: unknown) => {
        if (!isLatestGeneration(conversationId, generation)) return
        if (
          sessionHasPendingCancel(get().byConversationId.get(conversationId))
        ) {
          endDetailLoadingWithoutCommit(conversationId)
          return
        }
        const message = toErrorMessage(error)
        if (pendingDelegateTerminalSync.has(conversationId)) {
          pendingDelegateTerminalSync.delete(conversationId)
          dispatch({
            type: "SET_DELEGATE_SYNC_ERROR",
            conversationId,
            error: message,
          })
        }
        dispatch({
          type: "FETCH_DETAIL_ERROR",
          conversationId,
          error: message,
        })
      })
  }

  /** Drop the loading bit without installing detail (pending cancel fence). */
  const endDetailLoadingWithoutCommit = (conversationId: number): void => {
    set((state) => {
      const current = state.byConversationId.get(conversationId)
      if (!current || !current.detailLoading) return state
      return updateSessionInState(state, conversationId, (s) => ({
        ...s,
        detailLoading: false,
      }))
    })
  }

  /**
   * Raw non-dispatching detail read used only by the cancel coordinator.
   * Shares the same transport path as refetchDetail / viewer sync but never
   * dispatches FETCH_DETAIL_SUCCESS.
   */
  const rawFetchDetail = (
    conversationId: number
  ): Promise<DbConversationDetail> => {
    const session = get().byConversationId.get(conversationId)
    const fetchId = resolvePersistedConversationId(session, conversationId)
    return getFolderConversation(fetchId)
  }

  const clearCancelReconcile = (conversationId: number): void => {
    stopCancelReconcileTimers(conversationId)
    bumpCancelGeneration(conversationId)
    dispatch({ type: "CLEAR_CANCEL_RECONCILE", conversationId })
  }

  const recordTurnOutcome = (params: {
    conversationId: number
    connectionId: string
    completionSeq: number
    outcome: TurnOutcome
  }): void => {
    const { conversationId, connectionId, completionSeq, outcome } = params
    if (!get().byConversationId.has(conversationId)) return
    const outcomeKey = `${connectionId}\0${completionSeq}`
    if (recordedTurnOutcomeKeys.get(conversationId) === outcomeKey) return
    recordedTurnOutcomeKeys.set(conversationId, outcomeKey)
    dispatch({
      type: "RECORD_TURN_OUTCOME",
      conversationId,
      connectionId,
      completionSeq,
      outcome,
    })
  }

  const startCancelReconcile = (params: {
    conversationId: number
    connectionId: string
    completionSeq: number
    providerTurnId: string
    activeTurnToken?: string | null
  }): void => {
    const {
      conversationId,
      connectionId,
      completionSeq,
      providerTurnId,
      activeTurnToken: activeTurnTokenOpt,
    } = params
    const session = get().byConversationId.get(conversationId)
    if (!session) return
    // Start gates: non-empty provider id + positive persisted conversation id.
    if (!providerTurnId) return
    const persistedId = resolvePersistedConversationId(session, conversationId)
    if (!(persistedId > 0)) return

    // Idempotent when the same completion is already coordinating.
    const existing = session.pendingCancel
    if (
      existing &&
      existing.connectionId === connectionId &&
      existing.completionSeq === completionSeq &&
      existing.providerTurnId === providerTurnId
    ) {
      return
    }

    // Replace any prior in-flight coordinator for a different completion.
    stopCancelReconcileTimers(conversationId)
    // Cancel competing automatic detail owners so their in-flight responses
    // cannot commit after the fence is installed (commit-time rechecks remain).
    cancelViewerDetailSync(conversationId)
    delegateTerminalSyncCancels.get(conversationId)?.()

    const key: CancelCompletionKey = {
      conversationId,
      connectionId,
      completionSeq,
      activeTurnToken:
        activeTurnTokenOpt !== undefined
          ? activeTurnTokenOpt
          : session.activeTurnToken,
      providerTurnId,
      cancelGeneration: getCancelGeneration(conversationId),
    }
    dispatch({ type: "START_CANCEL_RECONCILE", conversationId, key })

    let cancelled = false
    let timer: ReturnType<typeof setTimeout> | null = null
    const cancel = (): void => {
      cancelled = true
      if (timer) {
        clearTimeout(timer)
        timer = null
      }
      if (cancelReconcileCancels.get(conversationId) === cancel) {
        cancelReconcileCancels.delete(conversationId)
      }
    }
    cancelReconcileCancels.set(conversationId, cancel)

    const gatesStillHold = (): boolean => {
      if (cancelled) return false
      const cur = get().byConversationId.get(conversationId)
      if (!cur?.pendingCancel) return false
      const p = cur.pendingCancel
      if (
        p.connectionId !== key.connectionId ||
        p.completionSeq !== key.completionSeq ||
        p.cancelGeneration !== key.cancelGeneration ||
        p.providerTurnId !== key.providerTurnId
      ) {
        return false
      }
      if (getCancelGeneration(conversationId) !== key.cancelGeneration) {
        return false
      }
      // When the key captured a non-null owner token, a newer prompt replaces it.
      if (
        key.activeTurnToken != null &&
        cur.activeTurnToken != null &&
        cur.activeTurnToken !== key.activeTurnToken
      ) {
        return false
      }
      return true
    }

    const finishExhausted = (): void => {
      if (cancelled) return
      // Terminal failure: clear key + bump generation; retain local content.
      stopCancelReconcileTimers(conversationId)
      bumpCancelGeneration(conversationId)
      dispatch({ type: "CLEAR_CANCEL_RECONCILE", conversationId })
    }

    const attempt = (index: number): void => {
      if (!gatesStillHold()) return
      timer = setTimeout(() => {
        timer = null
        if (!gatesStillHold()) return
        rawFetchDetail(conversationId)
          .then((detail) => {
            if (!gatesStillHold()) return
            if (!detailHasMatchingCancelFence(detail, key.providerTurnId)) {
              if (index + 1 < CANCEL_RECONCILE_DELAYS_MS.length) {
                attempt(index + 1)
              } else {
                finishExhausted()
              }
              return
            }
            // Fence matched — apply only via RECONCILE_CANCELLED_TURN.
            dispatch({
              type: "RECONCILE_CANCELLED_TURN",
              conversationId,
              detail,
              key,
            })
            stopCancelReconcileTimers(conversationId)
            bumpCancelGeneration(conversationId)
          })
          .catch(() => {
            // Transport/parse errors: retry without blocking banner.
            if (!gatesStillHold()) return
            if (index + 1 < CANCEL_RECONCILE_DELAYS_MS.length) {
              attempt(index + 1)
            } else {
              finishExhausted()
            }
          })
      }, CANCEL_RECONCILE_DELAYS_MS[index])
    }

    attempt(0)
  }

  const reloadDetail = (
    conversationId: number,
    options: { reason: "manual_reload" }
  ): void => {
    if (options.reason !== "manual_reload") return
    // Manual Reload override: clear fence before authoritative load.
    clearCancelReconcile(conversationId)
    const session = get().byConversationId.get(conversationId)
    if (!session) return
    const fetchId = resolvePersistedConversationId(session, conversationId)
    const generation = bumpFetchGeneration(conversationId)
    dispatch({ type: "FETCH_DETAIL_START", conversationId })
    getFolderConversation(fetchId)
      .then((detail) => {
        if (!isLatestGeneration(conversationId, generation)) return
        dispatch({
          type: "FETCH_DETAIL_SUCCESS",
          conversationId,
          detail,
          preserveLive: false,
        })
        afterDetailFetchSuccess(conversationId, detail)
      })
      .catch((error: unknown) => {
        if (!isLatestGeneration(conversationId, generation)) return
        dispatch({
          type: "FETCH_DETAIL_ERROR",
          conversationId,
          error: toErrorMessage(error),
        })
      })
  }

  // Bring a passively-VIEWED conversation's detail up to date after its turn
  // completed on another client. See `viewerDetailSyncCancels` above for why the
  // panel's live promotion never fires for such a viewer and why this must poll
  // rather than refetch once. No-op (returns immediately) unless the session is
  // open AND a pure viewer, so the owner's in-flight/just-completed reply is
  // never touched. Never sets `detailLoading` — a passive background sync must
  // not flash a spinner over the content the viewer is already reading.
  const syncViewerDetail = (nudgedConversationId: number): void => {
    // The nudge carries a positive DB id; map it to the runtime session key,
    // which may be a virtual negative id for a draft-originated tab (issue: the
    // `dbConversationId` fetch fallback below is unreachable without this).
    const conversationId = resolveViewerRuntimeId(
      get().byConversationId,
      nudgedConversationId
    )
    if (conversationId == null) return
    const session = get().byConversationId.get(conversationId)
    if (!session || !isPureViewerSession(session)) return
    // Exclusive path: pending cancel fence blocks unfenced viewer destructive sync.
    if (sessionHasPendingCancel(session)) return

    // Restart, don't stack: a fresh nudge supersedes any in-flight poll.
    cancelViewerDetailSync(conversationId)

    let cancelled = false
    let timer: ReturnType<typeof setTimeout> | null = null
    const cancel = (): void => {
      cancelled = true
      if (timer) clearTimeout(timer)
      if (viewerDetailSyncCancels.get(conversationId) === cancel) {
        viewerDetailSyncCancels.delete(conversationId)
      }
    }
    viewerDetailSyncCancels.set(conversationId, cancel)

    const attempt = (n: number): void => {
      if (cancelled) return
      const cur = get().byConversationId.get(conversationId)
      // The session vanished (tab closed) or started driving its own turn
      // (a local send / live stream) between ticks — stop; it is no longer a
      // pure viewer this poll may refetch under.
      if (!cur || !isPureViewerSession(cur)) {
        cancel()
        return
      }
      // Read the DB fetch id fresh each tick: a just-bound draft resolves its
      // `dbConversationId` asynchronously, and the runtime key alone is not
      // always fetchable (a virtual negative id). Falls back to the key.
      const fetchId = cur.dbConversationId ?? conversationId
      // `getFolderConversation` here can itself emit a `conversation://changed`
      // upsert (auto-title backfill), which re-enters this poll (cancel +
      // restart). That converges — the title only changes a bounded number of
      // times and the attempt cap bounds each run — but it's why this is the one
      // detail fetcher that both triggers and can re-trigger itself.
      const generation = bumpFetchGeneration(conversationId)
      getFolderConversation(fetchId)
        .then((detail) => {
          if (cancelled) return
          const cur2 = get().byConversationId.get(conversationId)
          if (!cur2 || !isPureViewerSession(cur2)) {
            cancel()
            return
          }
          // The generation gate governs only the COMMIT: a concurrent panel
          // fetch/refetch (or a superseding nudge) that bumped the counter owns
          // the detail now, so we must not clobber it with this (possibly older)
          // read — but we still evaluate convergence below and keep polling,
          // since that superseding read may have landed a pre-reply transcript.
          const isLatest = isLatestGeneration(conversationId, generation)
          // Convergence: keep polling while the reply isn't fully persisted:
          //  - `in_flight_user_turn_id` is the backend's authoritative "a turn
          //    is still running on this connection" flag (set from the pending
          //    user message, cleared at TurnComplete) — it also covers agents
          //    that persist a PARTIAL assistant turn mid-stream (OpenCode,
          //    Gemini), where a role check alone would stop early; and
          //  - a trailing USER turn means the turn ended (per the wire) but its
          //    assistant record hasn't flushed to the JSONL yet (Claude/Codex).
          // Any other settled tail (assistant reply, or no turns) means there is
          // nothing more to wait for.
          const lastTurn = detail.turns[detail.turns.length - 1]
          const replyPending =
            detail.in_flight_user_turn_id != null || lastTurn?.role === "user"
          // Skip a no-op dispatch (identical transcript) so a multi-tick poll
          // doesn't re-render the message list on every attempt. Compare the
          // cheap byte watermark + turn count rather than deep-diffing turns.
          // A FETCH_DETAIL_SUCCESS carrying the backend's in-flight stamp keeps
          // the synthesized viewer prompt (`keepAllLiveBuffers`); a settled load
          // replaces it — so "hi" only transiently disappears in the narrow case
          // where a non-in-flight read that lacks the just-sent prompt commits
          // between turns, and the next nudge re-surfaces it.
          const prev = cur2.detail
          const changed =
            !prev ||
            (prev.transcript_watermark ?? null) !==
              (detail.transcript_watermark ?? null) ||
            prev.turns.length !== detail.turns.length
          // Commit when the transcript advanced (`changed`) OR when the reply
          // just settled (`!replyPending`). The settle case lands the FINAL
          // content for a no-watermark agent that grows its partial assistant
          // turn IN PLACE (OpenCode/Gemini): its final read shares the partial's
          // null watermark and turn count, so `changed` alone would suppress it
          // and the poll would then stop, freezing the viewer on the partial.
          if (isLatest && (changed || !replyPending)) {
            // Re-check exclusive cancel fence before destructive commit.
            if (
              sessionHasPendingCancel(
                get().byConversationId.get(conversationId)
              )
            ) {
              cancel()
              return
            }
            dispatch({
              type: "FETCH_DETAIL_SUCCESS",
              conversationId,
              detail,
              preserveLive: false,
            })
          }
          if (replyPending && n + 1 < VIEWER_DETAIL_SYNC_DELAYS_MS.length) {
            timer = setTimeout(
              () => attempt(n + 1),
              VIEWER_DETAIL_SYNC_DELAYS_MS[n + 1]
            )
            return
          }
          cancel()
        })
        .catch(() => {
          // A failed read is transient (the transcript may be mid-write); retry
          // on the same schedule, then give up. Never surfaces a detailError —
          // the viewer keeps its current content.
          if (cancelled) return
          if (n + 1 < VIEWER_DETAIL_SYNC_DELAYS_MS.length) {
            timer = setTimeout(
              () => attempt(n + 1),
              VIEWER_DETAIL_SYNC_DELAYS_MS[n + 1]
            )
            return
          }
          cancel()
        })
    }

    attempt(0)
  }

  const syncDelegateTerminalDetail = (nudgedConversationId: number): void => {
    const conversationId = resolveViewerRuntimeId(
      get().byConversationId,
      nudgedConversationId
    )
    if (conversationId == null) return
    const initial = get().byConversationId.get(conversationId)
    if (!initial) return
    // Exclusive path: do not unfenced-replace while cancel fence is pending.
    if (sessionHasPendingCancel(initial)) return

    // Loaded non-delegate: never terminal-converge.
    if (initial.detail != null && initial.detail.summary.kind !== "delegate") {
      pendingDelegateTerminalSync.delete(conversationId)
      return
    }

    // Detail not seeded yet: queue terminal sync and ensure a preserveLive
    // seed fetch. In-flight plain fetchDetail commits also honor the pending
    // flag (see fetchDetail/refetchDetail). Without this, a terminal trigger
    // before first detail returns early and a later non-preserve commit can
    // permanently clear live/local buffers.
    if (initial.detail == null) {
      pendingDelegateTerminalSync.add(conversationId)
      if (initial.detailLoading) {
        // Wait for the in-flight fetch; success path re-enters this sync.
        return
      }
      // Start a seed even when live buffers would make fetchDetail no-op.
      cancelViewerDetailSync(conversationId)
      delegateTerminalSyncCancels.get(conversationId)?.()
      let cancelled = false
      const cancel = (): void => {
        cancelled = true
        if (delegateTerminalSyncCancels.get(conversationId) === cancel) {
          delegateTerminalSyncCancels.delete(conversationId)
        }
      }
      delegateTerminalSyncCancels.set(conversationId, cancel)
      const fetchId = initial.dbConversationId ?? conversationId
      const generation = bumpFetchGeneration(conversationId)
      getFolderConversation(fetchId)
        .then((detail) => {
          if (cancelled) return
          if (
            sessionHasPendingCancel(get().byConversationId.get(conversationId))
          ) {
            cancel()
            return
          }
          if (isLatestGeneration(conversationId, generation)) {
            dispatch({
              type: "FETCH_DETAIL_SUCCESS",
              conversationId,
              detail,
              preserveLive: true,
            })
          }
          if (pendingDelegateTerminalSync.has(conversationId)) {
            schedulePendingDelegateTerminalSync(conversationId)
          }
        })
        .catch((error: unknown) => {
          if (cancelled) return
          if (
            sessionHasPendingCancel(get().byConversationId.get(conversationId))
          ) {
            cancel()
            return
          }
          // Surface as terminal-sync failure; keep live content. Drop the
          // pending flag so a later explicit trigger can re-queue cleanly.
          pendingDelegateTerminalSync.delete(conversationId)
          dispatch({
            type: "SET_DELEGATE_SYNC_ERROR",
            conversationId,
            error: toErrorMessage(error),
          })
          cancel()
        })
      return
    }

    // Detail is a delegate — leave the pending queue and run convergence.
    pendingDelegateTerminalSync.delete(conversationId)

    cancelViewerDetailSync(conversationId)
    delegateTerminalSyncCancels.get(conversationId)?.()
    dispatch({
      type: "SET_DELEGATE_SYNC_ERROR",
      conversationId,
      error: null,
    })

    const anchor = captureDelegateTerminalSyncAnchor(get(), conversationId)
    let cancelled = false
    let timer: ReturnType<typeof setTimeout> | null = null
    const cancel = (): void => {
      cancelled = true
      if (timer) clearTimeout(timer)
      if (delegateTerminalSyncCancels.get(conversationId) === cancel) {
        delegateTerminalSyncCancels.delete(conversationId)
      }
    }
    delegateTerminalSyncCancels.set(conversationId, cancel)

    const committedDetailHasConverged = (): boolean => {
      const committed = get().byConversationId.get(conversationId)?.detail
      return (
        committed != null && delegateTerminalDetailConverged(committed, anchor)
      )
    }

    const fail = (message = DELEGATE_TERMINAL_SYNC_FAILED): void => {
      if (cancelled) return
      dispatch({
        type: "SET_DELEGATE_SYNC_ERROR",
        conversationId,
        error: message,
      })
      cancel()
    }

    const attempt = (index: number): void => {
      if (cancelled) return
      const current = get().byConversationId.get(conversationId)
      if (!current || current.detail?.summary.kind !== "delegate") {
        cancel()
        return
      }
      // Exclusive path: stop while a cancel fence is pending.
      if (sessionHasPendingCancel(current)) {
        cancel()
        return
      }
      const fetchId = current.dbConversationId ?? conversationId
      const generation = bumpFetchGeneration(conversationId)
      getFolderConversation(fetchId)
        .then((detail) => {
          if (cancelled) return
          const currentAfterRead = get().byConversationId.get(conversationId)
          if (!currentAfterRead) {
            cancel()
            return
          }
          // Recheck fence at commit time (fence may start mid-flight).
          if (sessionHasPendingCancel(currentAfterRead)) {
            cancel()
            return
          }
          const latest = isLatestGeneration(conversationId, generation)
          const converged = delegateTerminalDetailConverged(detail, anchor)
          if (latest) {
            dispatch({
              type: "FETCH_DETAIL_SUCCESS",
              conversationId,
              detail,
              preserveLive: !converged,
            })
          }
          if ((latest && converged) || committedDetailHasConverged()) {
            cancel()
            return
          }
          if (index + 1 >= VIEWER_DETAIL_SYNC_DELAYS_MS.length) {
            fail()
            return
          }
          timer = setTimeout(
            () => attempt(index + 1),
            VIEWER_DETAIL_SYNC_DELAYS_MS[index + 1]
          )
        })
        .catch((error: unknown) => {
          if (cancelled) return
          if (
            sessionHasPendingCancel(get().byConversationId.get(conversationId))
          ) {
            cancel()
            return
          }
          if (committedDetailHasConverged()) {
            cancel()
            return
          }
          if (index + 1 >= VIEWER_DETAIL_SYNC_DELAYS_MS.length) {
            fail(toErrorMessage(error))
            return
          }
          timer = setTimeout(
            () => attempt(index + 1),
            VIEWER_DETAIL_SYNC_DELAYS_MS[index + 1]
          )
        })
    }

    attempt(0)
  }
  resumeDelegateTerminalSync = syncDelegateTerminalDetail

  const syncTurnMetadata = (
    dbConversationId: number,
    runtimeConversationId?: number
  ): (() => void) => {
    const runtimeId = runtimeConversationId ?? dbConversationId
    let cancelled = false
    let timerId: ReturnType<typeof setTimeout> | null = null

    const trySync = (attempt: number) => {
      const delay = attempt === 0 ? 1500 : 3000
      timerId = setTimeout(() => {
        if (cancelled) return
        const session = get().byConversationId.get(runtimeId)
        if (!session || session.localTurns.length === 0) return
        if (session.syncState === "awaiting_persist") return

        getFolderConversation(dbConversationId)
          .then((parsed) => {
            if (cancelled) return
            const cur = get().byConversationId.get(runtimeId)
            if (!cur || cur.localTurns.length === 0) return
            if (cur.syncState === "awaiting_persist") return

            const localAssistantIndices: number[] = []
            for (let i = 0; i < cur.localTurns.length; i++) {
              if (cur.localTurns[i].role === "assistant") {
                localAssistantIndices.push(i)
              }
            }

            const parsedAssistantTurns = parsed.turns.filter(
              (t) => t.role === "assistant"
            )
            // Persisted history lives in `detail`, not `localTurns`; the fresh
            // parse returns history + this session's turns. The boundary,
            // captured at send time, tells the alignment how many leading
            // parsed turns are history so it never folds one into the first
            // resumed reply. `null` (no optimistic-initiated batch, e.g. the
            // sub-agent adopt path) means treat the whole parse as this
            // session's — the pre-capture behavior, correct when `localTurns`
            // overlaps the parse tail.
            const persistedAssistantCount = cur.historyAssistantBaseline ?? 0
            const patches = computeTurnMetadataPatches({
              localAssistantIndices,
              parsedAssistantTurns,
              persistedAssistantCount,
            })

            // Preserve broker-stamped tool metadata (including delegation
            // task previews/runtime projection) using the same history-aware
            // alignment as the scalar metadata helper above.
            const historyBoundary = Math.min(
              Math.max(persistedAssistantCount, 0),
              parsedAssistantTurns.length
            )
            const sessionParsedTurns =
              parsedAssistantTurns.slice(historyBoundary)
            const offset =
              sessionParsedTurns.length - localAssistantIndices.length
            for (let i = 0; i < localAssistantIndices.length; i++) {
              const parsedIdx = Math.max(offset, 0) + i
              const matchedTurns =
                parsedIdx >= 0 && parsedIdx < sessionParsedTurns.length
                  ? [sessionParsedTurns[parsedIdx]]
                  : []
              const toolMetaTurns =
                i === 0 && offset > 0
                  ? [...sessionParsedTurns.slice(0, offset), ...matchedTurns]
                  : matchedTurns
              const index = localAssistantIndices[i]
              const toolMeta = collectToolMetaPatches(
                cur.localTurns[index],
                toolMetaTurns
              )
              if (toolMeta.length === 0) continue
              const existing = patches.find((patch) => patch.index === index)
              if (existing) existing.tool_meta = toolMeta
              else patches.push({ index, tool_meta: toolMeta })
            }

            if (patches.length > 0 || parsed.session_stats) {
              dispatch({
                type: "PATCH_TURN_METADATA",
                conversationId: runtimeId,
                turnPatches: patches,
                sessionStats: parsed.session_stats,
              })
            }

            // Retry once if the MOST RECENT local assistant turn still lacks
            // usage — its transcript may not have flushed yet. Keying on the
            // last EMITTED patch is wrong when the latest local turn is the
            // unflushed one: an earlier reply's patch (with usage) would
            // suppress the retry the latest turn needs.
            const lastLocalAssistantIndex =
              localAssistantIndices[localAssistantIndices.length - 1]
            const latestCoverage =
              lastLocalAssistantIndex === undefined
                ? undefined
                : patches.find((p) => p.index === lastLocalAssistantIndex)
            if (
              lastLocalAssistantIndex !== undefined &&
              !latestCoverage?.usage &&
              attempt < 1
            ) {
              trySync(attempt + 1)
            }
          })
          .catch(() => {
            // Silent — localTurns content remains visible
          })
      }, delay)
    }

    trySync(0)

    return () => {
      cancelled = true
      if (timerId) clearTimeout(timerId)
    }
  }

  const actions: RuntimeActions = {
    fetchDetail,
    refetchDetail,
    reloadDetail,
    recordTurnOutcome,
    startCancelReconcile,
    clearCancelReconcile,
    syncViewerDetail,
    syncDelegateTerminalDetail,
    syncTurnMetadata,
    completeTurn: (conversationId, liveMessage) => {
      // Deliberately NO refetchDetail here (tried and reverted — see git
      // history). It used to exist
      // to fold a held-open turn's (claude-agent-acp v0.59.0's #870) content
      // into the persisted view, since the backend transcript watcher had no
      // visibility into what the wire already rendered. That's no longer
      // needed: `background_watch.rs` suppresses the overlay turn for a held
      // turn's own launched tasks, and the async sub-agent launch card is now
      // flipped in-memory from the `settled` event (RESOLVE_BACKGROUND_TASK /
      // the COMPLETE_TURN drain below) — so there's nothing left for a
      // post-completion refetch to reconcile. Worse, the refetch actively lost
      // content: it races the transcript file's own last write against this
      // very `TurnComplete` event — real hardware evidence showed the final
      // assistant record's timestamp only 8ms before turn_complete fired, well
      // inside the file-flush's own margin — and `preserveLive: false`
      // unconditionally discarded the already-correct `localTurns`/`liveMessage`
      // in favor of whatever that (sometimes-incomplete) fresh read returned,
      // visibly dropping the turn's trailing content. The dispatch below already
      // promotes `liveMessage`/`optimisticTurns` into `localTurns`
      // synchronously, with no read from disk and therefore no race — that IS
      // the complete, correct render; a later cold detail fetch (opening the tab
      // again, etc.) reconciles it against the DB whenever that naturally
      // happens.
      dispatch({ type: "COMPLETE_TURN", conversationId, liveMessage })
    },
    appendOptimisticTurn: (conversationId, turn, turnToken) => {
      // New prompt: cancel coordinator timers + bump cancel generation.
      stopCancelReconcileTimers(conversationId)
      bumpCancelGeneration(conversationId)
      dispatch({
        type: "APPEND_OPTIMISTIC_TURN",
        conversationId,
        turn,
        turnToken,
      })
    },
    removeOptimisticTurn: (conversationId, id) =>
      dispatch({ type: "REMOVE_OPTIMISTIC_TURN", conversationId, id }),
    appendViewerUserTurn: (conversationId, turn) => {
      const sessionBefore = get().byConversationId.get(conversationId)
      // Exact-id sender-echo must keep cancel timers + ownership. Every other
      // path is a real co-controller/viewer prompt (visible append or
      // same-text content-dedup under a new id) and must invalidate even when
      // pendingCancel is still null (Stop before typed user_stop envelope).
      const exactIdEcho = isExactIdViewerUserEcho(sessionBefore, turn.id)
      dispatch({ type: "APPEND_VIEWER_USER_TURN", conversationId, turn })
      if (!exactIdEcho) {
        stopCancelReconcileTimers(conversationId)
        bumpCancelGeneration(conversationId)
      }
    },
    applyBackgroundActivity: (conversationId, turns, watermark) =>
      dispatch({
        type: "APPLY_BACKGROUND_ACTIVITY",
        conversationId,
        turns,
        watermark,
      }),
    resolveBackgroundTask: (conversationId, settlement) =>
      dispatch({
        type: "RESOLVE_BACKGROUND_TASK",
        conversationId,
        settlement,
      }),
    setLiveMessage: (conversationId, liveMessage, isLive, deliveryIds) => {
      dispatch({
        type: "SET_LIVE_MESSAGE",
        conversationId,
        liveMessage,
        isLive,
      })
      // Queue after the Zustand update; the desktop listener flushes after
      // markTransactionComplete so stage order stays receipt→tx→live→paint.
      // Prefer explicit IDs, else the process-local in-flight delivery binding.
      // Skip entirely when inactive so the hot path stays a single boolean check.
      if (!streamingPerfRecorder.isActive()) return
      const ids =
        deliveryIds && deliveryIds.length > 0
          ? deliveryIds
          : streamingPerfRecorder.getCurrentDeliveryIds()
      if (ids && ids.length > 0) {
        streamingPerfRecorder.queueLivePublication(ids)
      }
    },
    setExternalId: (conversationId, externalId) => {
      const prev = get().byConversationId.get(conversationId)?.externalId
      if (prev != null && prev !== externalId) {
        stopCancelReconcileTimers(conversationId)
        bumpCancelGeneration(conversationId)
      }
      dispatch({ type: "SET_EXTERNAL_ID", conversationId, externalId })
    },
    setDbConversationId: (conversationId, dbConversationId) => {
      const prev = get().byConversationId.get(conversationId)?.dbConversationId
      if (prev != null && prev !== dbConversationId) {
        stopCancelReconcileTimers(conversationId)
        bumpCancelGeneration(conversationId)
      }
      dispatch({
        type: "SET_DB_CONVERSATION_ID",
        conversationId,
        dbConversationId,
      })
    },
    setSyncState: (conversationId, syncState) =>
      dispatch({ type: "SET_SYNC_STATE", conversationId, syncState }),
    migrateConversation: (fromConversationId, toConversationId) => {
      stopCancelReconcileTimers(fromConversationId)
      stopCancelReconcileTimers(toConversationId)
      // Carry user_stop ownership with the session. Without this, a late
      // envelope that resolves the new id finds no record and is treated as
      // current. Keep the from-id entry as a tombstone so late envelopes still
      // keyed on the old id are stale once the session map entry is gone.
      const fromOwnership = userStopOwnershipById.get(fromConversationId)
      if (fromOwnership) {
        userStopOwnershipById.set(toConversationId, fromOwnership)
      }
      // Independent +1 on each key can re-sync destination to the copied
      // snapshot (draft appendOptimisticTurn leaves from=1; fresh to 0→1).
      // Assign both keys max(from,to)+1 so the counter is strictly past any
      // carried ownership.cancelGeneration (and past both prior counters).
      const nextCancelGen =
        Math.max(
          getCancelGeneration(fromConversationId),
          getCancelGeneration(toConversationId)
        ) + 1
      cancelGenerationById.set(fromConversationId, nextCancelGen)
      cancelGenerationById.set(toConversationId, nextCancelGen)
      recordedTurnOutcomeKeys.delete(fromConversationId)
      recordedTurnOutcomeKeys.delete(toConversationId)
      dispatch({
        type: "MIGRATE_CONVERSATION",
        fromConversationId,
        toConversationId,
      })
      liveTranscriptStore.migrate(fromConversationId, toConversationId)
    },
    setPendingCleanup: (conversationId, pendingCleanup) =>
      dispatch({ type: "SET_PENDING_CLEANUP", conversationId, pendingCleanup }),
    setAcpLoadError: (conversationId, error) =>
      dispatch({ type: "SET_ACP_LOAD_ERROR", conversationId, error }),
    setLiveOwnsActiveTurn: (conversationId, value, kickoffText) =>
      dispatch({
        type: "SET_LIVE_OWNS_ACTIVE_TURN",
        conversationId,
        value,
        kickoffText,
      }),
    removeConversation: (conversationId) => {
      // Invalidate any outstanding fetch for this conversation so a
      // late-arriving response can't resurrect the session with stale
      // detail. See `fetchGeneration` above.
      bumpFetchGeneration(conversationId)
      stopCancelReconcileTimers(conversationId)
      bumpCancelGeneration(conversationId)
      recordedTurnOutcomeKeys.delete(conversationId)
      userStopOwnershipById.delete(conversationId)
      // Stop a viewer-sync poll whose tab just closed (its own tick guard would
      // also stop it on the next fire, but cancelling now drops the pending
      // timer immediately).
      cancelViewerDetailSync(conversationId)
      delegateTerminalSyncCancels.get(conversationId)?.()
      pendingDelegateTerminalSync.delete(conversationId)
      dispatch({ type: "REMOVE_CONVERSATION", conversationId })
      liveTranscriptStore.remove(conversationId)
    },
    reset: () => {
      cancelAllDetailSyncs()
      cancelGenerationById.clear()
      recordedTurnOutcomeKeys.clear()
      dispatch({ type: "RESET" })
      liveTranscriptStore.reset()
    },
  }

  return {
    byConversationId: new Map<number, ConversationRuntimeSession>(),
    conversationIdByExternalId: new Map<string, number>(),
    actions,
  }
})

// ─── Read helpers ────────────────────────────────────────────────────────
// Callbacks/effects read fresh state through `getState()` (no subscription);
// components subscribe via `useConversationRuntimeStore(selector)` instead.

/** Current runtime session for a conversation, or null. */
export function getRuntimeSession(
  conversationId: number
): ConversationRuntimeSession | null {
  return (
    useConversationRuntimeStore
      .getState()
      .byConversationId.get(conversationId) ?? null
  )
}

/** Resolve a runtime conversation id from an agent's external session id. */
export function getConversationIdByExternalIdFromStore(
  externalId: string
): number | null {
  return (
    useConversationRuntimeStore
      .getState()
      .conversationIdByExternalId.get(externalId) ?? null
  )
}

/** Derived render timeline for a conversation (reads current state). */
export function getTimelineTurns(
  conversationId: number
): ConversationTimelineTurn[] {
  return computeTimeline(useConversationRuntimeStore.getState(), conversationId)
}

/**
 * Historical timeline only (no streaming phase). Cached with a key that
 * excludes live content so content-only live appends return the exact same
 * array and entry references.
 */
export function selectHistoricalTimelineTurns(
  state: ConversationRuntimeState,
  conversationId: number
): ConversationTimelineTurn[] {
  return computeHistoricalTimeline(state, conversationId)
}

/**
 * State-taking timeline selector for `useConversationRuntimeStore(...)`.
 * Compatibility path: stable historical rows plus canonical live streaming
 * turns for consumers not yet migrated to the historical/live split.
 * Historical entries stay reference-stable across content-only live updates;
 * the returned outer array changes when streaming content is present.
 */
export function selectTimelineTurns(
  state: ConversationRuntimeState,
  conversationId: number
): ConversationTimelineTurn[] {
  return computeTimeline(state, conversationId)
}

/**
 * Public selector for store-backed read-only delegation activities.
 * Updated on SET_LIVE_MESSAGE, COMPLETE_TURN, and settled FETCH_DETAIL_SUCCESS.
 * Always returns a stable empty reference when absent (avoids getSnapshot loops).
 */
export function selectDelegationActivities(
  state: { byConversationId: Map<number, ConversationRuntimeSession> },
  conversationId: number
): DelegationActivityView[] {
  const activities =
    state.byConversationId.get(conversationId)?.delegationActivities
  if (!activities || activities.length === 0) {
    return EMPTY_DELEGATION_ACTIVITIES
  }
  return activities
}

/** Stable action bundle — reference never changes (reducer merges only the
 *  state slices, never `actions`), so consumers re-render zero times. */
export function useConversationRuntimeActions(): RuntimeActions {
  return useConversationRuntimeStore((s) => s.actions)
}

/**
 * One call-stack live→local handoff: mark live completing → promote the
 * canonical turn via `completeTurn` → drop the matching live projection.
 * React external-store notifications issued in this stack produce one
 * committed tree (no blank/duplicate assistant frame). Existing COMPLETE_TURN
 * idempotency guards are preserved.
 */
export function completeLiveTranscriptTurn(
  conversationId: number,
  liveMessage?: LiveMessage | null
): void {
  const live = liveTranscriptStore.getConversation(conversationId)
  if (live) {
    liveTranscriptStore.markCompleting(conversationId, live.messageId)
    // Complete each text partition and cache by exact canonical text for
    // one-shot historical TextPart handoff before the live projection is dropped.
    for (const id of live.segmentIds) {
      const segment = live.segments.get(id)
      if (segment?.type !== "text") continue
      const completed = completeStreamingMarkdown(segment.document)
      if (!completed.valid) continue
      const joined = joinStreamingMarkdown(completed)
      if (joined !== segment.text) continue
      cacheCompletedStreamingPartition(segment.text, completed)
    }
  }
  useConversationRuntimeStore
    .getState()
    .actions.completeTurn(conversationId, liveMessage)
  if (live) {
    liveTranscriptStore.removeIfMessage(conversationId, live.messageId)
  }
}

/**
 * Reset store state + module-level caches to a clean slate. Used by tests, and
 * by the backend-scoped reset registry if a realm's backend identity ever
 * changes (an invariant-violating transition that does not occur today — see
 * `RemoteConnectionGate`). In normal operation the store lives for the window's
 * lifetime and is never reset.
 */
export function resetConversationRuntimeStore(): void {
  // NOTE: clearing (vs. epoch-bumping) means a pre-reset in-flight fetch could
  // re-match a post-reset generation and commit stale detail. Harmless today —
  // the only production caller (the backend-identity guard) never fires and tests
  // have no concurrent fetches — but a real in-place backend switch would need a
  // backend epoch here. See `RemoteConnectionGate`.
  fetchGeneration.clear()
  cancelGenerationById.clear()
  recordedTurnOutcomeKeys.clear()
  userStopOwnershipById.clear()
  cancelAllDetailSyncs()
  historicalTimelineCache.clear()
  clearCompletedStreamingPartitions()
  useConversationRuntimeStore.setState({
    byConversationId: new Map(),
    conversationIdByExternalId: new Map(),
  })
}

// Reset this backend-scoped store on any (currently-unreachable) in-realm
// backend switch. See `backend-scoped-store-reset.ts`.
registerBackendScopedStoreReset(resetConversationRuntimeStore)
