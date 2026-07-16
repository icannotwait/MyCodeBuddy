/**
 * Live transcript external store (UI projection only).
 *
 * Narrow per-conversation / per-segment / per-tool subscriptions via
 * `useSyncExternalStore`. Canonical `LiveMessage` remains the source of truth
 * for reconnect, completion, and persistence.
 */

"use client"

import { useCallback, useSyncExternalStore } from "react"
import type {
  LiveMessage,
  ToolCallInfo,
} from "@/contexts/acp-connections-context"
import type { AcceptedConnectionFrame } from "@/lib/types"
import {
  applyLiveTranscriptEvents,
  projectLiveSnapshot,
  type LiveTranscriptSegment,
  type LiveTranscriptSnapshot,
} from "@/lib/acp/live-transcript-projector"
import {
  classifyToolKind,
  isAgentLikeToolName,
  TOOL_KIND_ORDER,
  type ToolKindLabel,
} from "@/lib/adapters/tool-kind-classifier"
import { resetStreamingPerformanceCaches } from "@/lib/acp/streaming-performance-config"
import { clearCompletedStreamingPartitions } from "@/lib/markdown/incremental-stream-blocks"
import { isPlanModeToolName } from "@/lib/plan-parse"
import { inferLiveToolName } from "@/lib/tool-call-normalization"
import { registerBackendScopedStoreReset } from "@/stores/backend-scoped-store-reset"

export type {
  LiveTranscriptSegment,
  LiveTranscriptSnapshot,
} from "@/lib/acp/live-transcript-projector"

/** Cheap aggregate for a structural run of consecutive live tools. */
export interface LiveToolGroupSummary {
  id: string
  toolCallIds: readonly string[]
  counts: Readonly<Record<ToolKindLabel, number>>
  runningCount: number
  errorCount: number
}

/** Visible running-output tail (matches generic tool card 24k bound). */
export const LIVE_RUNNING_OUTPUT_TAIL_CHARS = 24_000

/**
 * Join tool raw-output chunks (or content fallback) and return the trailing
 * `maxChars` characters. Walks chunks from the end so join cost tracks the
 * 24k bound instead of the full retained stream.
 */
export function selectRunningOutputTail(
  record: Pick<ToolCallInfo, "raw_output_chunks" | "content">,
  maxChars: number = LIVE_RUNNING_OUTPUT_TAIL_CHARS
): string {
  if (maxChars <= 0) return ""
  const chunks = record.raw_output_chunks
  if (chunks.length === 0) {
    const raw = record.content ?? ""
    return raw.length <= maxChars ? raw : raw.slice(-maxChars)
  }
  // Accumulate from the end until we cover maxChars (plus at most one extra
  // chunk), then join that suffix only.
  let total = 0
  const tail: string[] = []
  for (let i = chunks.length - 1; i >= 0; i--) {
    const chunk = chunks[i] ?? ""
    tail.push(chunk)
    total += chunk.length
    if (total >= maxChars) break
  }
  tail.reverse()
  const joined = tail.join("")
  return joined.length <= maxChars ? joined : joined.slice(-maxChars)
}

/** Join all retained raw-output chunks (or content) for equality tests. */
export function getToolJoinedOutput(
  record: Pick<ToolCallInfo, "raw_output_chunks" | "content">
): string {
  if (record.raw_output_chunks.length > 0) {
    return record.raw_output_chunks.join("")
  }
  return record.content ?? ""
}

function emptyKindCounts(): Record<ToolKindLabel, number> {
  return TOOL_KIND_ORDER.reduce(
    (acc, kind) => {
      acc[kind] = 0
      return acc
    },
    {} as Record<ToolKindLabel, number>
  )
}

function liveToolName(info: ToolCallInfo): string {
  return inferLiveToolName({
    title: info.title,
    kind: info.kind,
    rawInput: info.raw_input,
    meta: info.meta,
  })
}

function isGroupableLiveTool(info: ToolCallInfo): boolean {
  const name = liveToolName(info)
  if (isAgentLikeToolName(name) || isPlanModeToolName(name)) return false
  // Background-task polls own a dedicated card — same break as historical.
  const lower = name.trim().toLowerCase()
  if (
    lower === "taskoutput" ||
    lower === "taskstop" ||
    lower === "task_output" ||
    lower === "task_stop"
  ) {
    return false
  }
  return true
}

function isToolRunning(status: string): boolean {
  return status === "pending" || status === "in_progress"
}

function isToolError(status: string): boolean {
  return status === "failed"
}

/**
 * Build structural tool-group summaries from consecutive groupable tools in
 * segment order (mirrors historical `groupConsecutiveToolCalls`).
 */
export function buildLiveToolGroupSummaries(
  snapshot: LiveTranscriptSnapshot
): Map<string, LiveToolGroupSummary> {
  const groups = new Map<string, LiveToolGroupSummary>()
  let buffer: ToolCallInfo[] = []
  let groupIndex = 0

  const flush = () => {
    if (buffer.length === 0) return
    const toolCallIds = buffer.map((t) => t.tool_call_id)
    const counts = emptyKindCounts()
    let runningCount = 0
    let errorCount = 0
    for (const info of buffer) {
      counts[classifyToolKind(liveToolName(info))] += 1
      if (isToolRunning(info.status)) runningCount += 1
      if (isToolError(info.status)) errorCount += 1
    }
    const id = `tg:${snapshot.messageId}:${groupIndex++}`
    groups.set(id, {
      id,
      toolCallIds: Object.freeze([...toolCallIds]),
      counts,
      runningCount,
      errorCount,
    })
    buffer = []
  }

  for (const segmentId of snapshot.segmentIds) {
    const segment = snapshot.segments.get(segmentId)
    if (!segment || segment.type !== "tool") {
      flush()
      continue
    }
    const info = snapshot.tools.get(segment.toolCallId)
    if (!info || !isGroupableLiveTool(info)) {
      flush()
      continue
    }
    buffer.push(info)
  }
  flush()
  return groups
}

function toolGroupSummaryEqual(
  a: LiveToolGroupSummary,
  b: LiveToolGroupSummary
): boolean {
  if (a.runningCount !== b.runningCount) return false
  if (a.errorCount !== b.errorCount) return false
  if (a.toolCallIds.length !== b.toolCallIds.length) return false
  for (let i = 0; i < a.toolCallIds.length; i++) {
    if (a.toolCallIds[i] !== b.toolCallIds[i]) return false
  }
  for (const kind of TOOL_KIND_ORDER) {
    if (a.counts[kind] !== b.counts[kind]) return false
  }
  return true
}

function groupsEqual(
  a: Map<string, LiveToolGroupSummary>,
  b: Map<string, LiveToolGroupSummary>
): boolean {
  if (a.size !== b.size) return false
  for (const [id, ga] of a) {
    const gb = b.get(id)
    if (!gb) return false
    if (!toolGroupSummaryEqual(ga, gb)) return false
  }
  return true
}

/**
 * Rebuild group summaries while reusing previous summary object refs (and
 * toolCallIds arrays) when values are unchanged, so per-group subscribers
 * only wake when their group actually changed.
 */
function reuseToolGroupSummaries(
  prev: Map<string, LiveToolGroupSummary> | null,
  computed: Map<string, LiveToolGroupSummary>
): Map<string, LiveToolGroupSummary> {
  if (!prev || prev.size === 0) return computed
  let reusedAny = false
  const next = new Map<string, LiveToolGroupSummary>()
  for (const [id, summary] of computed) {
    const prior = prev.get(id)
    if (prior && toolGroupSummaryEqual(prior, summary)) {
      next.set(id, prior)
      reusedAny = true
    } else {
      next.set(id, summary)
    }
  }
  return reusedAny ? next : computed
}

/** Frame sink registered on a connection context key. */
export interface LiveTranscriptFrameSink {
  rebuild(canonical: LiveMessage, lastAppliedSeq: number): void
  publish(frame: AcceptedConnectionFrame, canonical: LiveMessage): void
  markCompleting(messageId: string): void
  clear(messageId: string): void
}

export interface LiveTranscriptProjectorApi {
  projectLiveSnapshot: typeof projectLiveSnapshot
  applyLiveTranscriptEvents: typeof applyLiveTranscriptEvents
}

export interface LiveTranscriptStoreApi {
  getConversation(conversationId: number): LiveTranscriptSnapshot | null
  getSegment(
    conversationId: number,
    segmentId: string
  ): LiveTranscriptSegment | null
  getTool(conversationId: number, toolCallId: string): ToolCallInfo | null
  getToolGroup(
    conversationId: number,
    groupId: string
  ): LiveToolGroupSummary | null
  getToolGroupIds(conversationId: number): readonly string[]
  subscribeConversation(
    conversationId: number,
    callback: () => void
  ): () => void
  subscribeSegment(
    conversationId: number,
    segmentId: string,
    callback: () => void
  ): () => void
  subscribeTool(
    conversationId: number,
    toolCallId: string,
    callback: () => void
  ): () => void
  subscribeToolGroup(
    conversationId: number,
    groupId: string,
    callback: () => void
  ): () => void
  rebuild(
    conversationId: number,
    connectionId: string,
    canonical: LiveMessage,
    cursor: number
  ): void
  publish(
    conversationId: number,
    frame: AcceptedConnectionFrame,
    canonical: LiveMessage
  ): void
  markCompleting(conversationId: number, messageId: string): void
  removeIfMessage(conversationId: number, messageId: string): void
  remove(conversationId: number): void
  migrate(fromConversationId: number, toConversationId: number): void
  getDebugStats(): {
    rebuildCount: number
    conversations: number
    segments: number
    tools: number
    toolGroups: number
  }
  reset(): void
}

const STABLE_NULL: null = null
const STABLE_EMPTY_IDS: readonly string[] = Object.freeze([])

function listenerKey(
  conversationId: number,
  kind: "conversation" | "segment" | "tool" | "group",
  id?: string
): string {
  if (kind === "conversation") return `c:${conversationId}`
  if (kind === "group") return `g:${conversationId}:${id ?? ""}`
  return `${kind[0]}:${conversationId}:${id ?? ""}`
}

export interface CreateLiveTranscriptStoreOptions {
  projector?: LiveTranscriptProjectorApi
}

export function createLiveTranscriptStore(
  options: CreateLiveTranscriptStoreOptions = {}
): LiveTranscriptStoreApi {
  const projector: LiveTranscriptProjectorApi = options.projector ?? {
    projectLiveSnapshot,
    applyLiveTranscriptEvents,
  }

  const conversations = new Map<number, LiveTranscriptSnapshot>()
  /** Structural tool-group summaries keyed by conversation. */
  const toolGroupsByConversation = new Map<
    number,
    Map<string, LiveToolGroupSummary>
  >()
  /** Stable group-id lists (same ref until group set identity changes). */
  const toolGroupIdsByConversation = new Map<number, readonly string[]>()
  const listeners = new Map<string, Set<() => void>>()
  /** Recovery rebuilds only (projector exception during publish). */
  let rebuildCount = 0

  function notify(key: string): void {
    const set = listeners.get(key)
    if (!set) return
    for (const cb of set) {
      try {
        cb()
      } catch (err) {
        console.error("[live-transcript-store] listener threw:", err)
      }
    }
  }

  function subscribe(key: string, callback: () => void): () => void {
    let set = listeners.get(key)
    if (!set) {
      set = new Set()
      listeners.set(key, set)
    }
    set.add(callback)
    return () => {
      set!.delete(callback)
      if (set!.size === 0) listeners.delete(key)
    }
  }

  function notifyConversation(conversationId: number): void {
    notify(listenerKey(conversationId, "conversation"))
  }

  function notifySegment(conversationId: number, segmentId: string): void {
    notify(listenerKey(conversationId, "segment", segmentId))
  }

  function notifyTool(conversationId: number, toolCallId: string): void {
    notify(listenerKey(conversationId, "tool", toolCallId))
  }

  function notifyToolGroup(conversationId: number, groupId: string): void {
    notify(listenerKey(conversationId, "group", groupId))
  }

  function syncToolGroups(
    conversationId: number,
    next: LiveTranscriptSnapshot,
    prevGroups: Map<string, LiveToolGroupSummary> | null
  ): void {
    const computed = buildLiveToolGroupSummaries(next)
    if (prevGroups && groupsEqual(prevGroups, computed)) {
      // Keep previous map ref when nothing changed so subscribers stay cold.
      return
    }
    // Per-group value reuse: unchanged groups keep the same summary ref so
    // `a !== b` only notifies groups that actually changed.
    const reused = reuseToolGroupSummaries(prevGroups, computed)
    toolGroupsByConversation.set(conversationId, reused)

    const prevIds = toolGroupIdsByConversation.get(conversationId)
    let nextIds: readonly string[]
    if (reused.size === 0) {
      nextIds = STABLE_EMPTY_IDS
    } else {
      const keys = [...reused.keys()]
      const sameIds =
        prevIds != null &&
        prevIds.length === keys.length &&
        prevIds.every((id, i) => id === keys[i])
      nextIds = sameIds ? prevIds : Object.freeze(keys)
    }
    toolGroupIdsByConversation.set(conversationId, nextIds)

    const allIds = new Set([...(prevGroups?.keys() ?? []), ...reused.keys()])
    for (const id of allIds) {
      const a = prevGroups?.get(id)
      const b = reused.get(id)
      if (a !== b) notifyToolGroup(conversationId, id)
    }
  }

  function diffAndNotify(
    conversationId: number,
    prev: LiveTranscriptSnapshot | null,
    next: LiveTranscriptSnapshot
  ): void {
    const prevGroups = toolGroupsByConversation.get(conversationId) ?? null

    // Always notify conversation once when the snapshot object changes.
    notifyConversation(conversationId)

    if (!prev) {
      for (const id of next.segmentIds) {
        notifySegment(conversationId, id)
      }
      for (const toolId of next.tools.keys()) {
        notifyTool(conversationId, toolId)
      }
      syncToolGroups(conversationId, next, prevGroups)
      return
    }

    const allSegmentIds = new Set([...prev.segmentIds, ...next.segmentIds])
    for (const id of allSegmentIds) {
      const a = prev.segments.get(id)
      const b = next.segments.get(id)
      if (a !== b) notifySegment(conversationId, id)
    }

    const allToolIds = new Set([...prev.tools.keys(), ...next.tools.keys()])
    for (const id of allToolIds) {
      if (prev.tools.get(id) !== next.tools.get(id)) {
        notifyTool(conversationId, id)
      }
    }

    // Status/create changes may only touch tools map — still refresh groups.
    if (
      prev.tools !== next.tools ||
      prev.segmentIds !== next.segmentIds ||
      prev.segments !== next.segments
    ) {
      syncToolGroups(conversationId, next, prevGroups)
    }
  }

  function setConversation(
    conversationId: number,
    next: LiveTranscriptSnapshot,
    prev: LiveTranscriptSnapshot | null
  ): void {
    if (prev === next) return
    // Skip notify when nothing material changed (same refs for structure).
    if (
      prev &&
      prev.segmentIds === next.segmentIds &&
      prev.segments === next.segments &&
      prev.tools === next.tools &&
      prev.status === next.status &&
      prev.messageId === next.messageId &&
      prev.lastAppliedSeq === next.lastAppliedSeq &&
      prev.connectionId === next.connectionId
    ) {
      return
    }
    conversations.set(conversationId, next)
    diffAndNotify(conversationId, prev, next)
  }

  const api: LiveTranscriptStoreApi = {
    getConversation(conversationId) {
      return conversations.get(conversationId) ?? null
    },

    getSegment(conversationId, segmentId) {
      return conversations.get(conversationId)?.segments.get(segmentId) ?? null
    },

    getTool(conversationId, toolCallId) {
      return conversations.get(conversationId)?.tools.get(toolCallId) ?? null
    },

    getToolGroup(conversationId, groupId) {
      return toolGroupsByConversation.get(conversationId)?.get(groupId) ?? null
    },

    getToolGroupIds(conversationId) {
      return toolGroupIdsByConversation.get(conversationId) ?? STABLE_EMPTY_IDS
    },

    subscribeConversation(conversationId, callback) {
      return subscribe(listenerKey(conversationId, "conversation"), callback)
    },

    subscribeSegment(conversationId, segmentId, callback) {
      return subscribe(
        listenerKey(conversationId, "segment", segmentId),
        callback
      )
    },

    subscribeTool(conversationId, toolCallId, callback) {
      return subscribe(
        listenerKey(conversationId, "tool", toolCallId),
        callback
      )
    },

    subscribeToolGroup(conversationId, groupId, callback) {
      return subscribe(listenerKey(conversationId, "group", groupId), callback)
    },

    rebuild(conversationId, connectionId, canonical, cursor) {
      const prev = conversations.get(conversationId) ?? null
      const next = projector.projectLiveSnapshot(
        conversationId,
        connectionId,
        canonical,
        cursor
      )
      setConversation(conversationId, next, prev)
    },

    publish(conversationId, frame, canonical) {
      const prev = conversations.get(conversationId) ?? null
      const connectionId = frame.connectionId

      // New turn / first publish / message identity change → full rebuild.
      if (!prev || prev.messageId !== canonical.id) {
        const next = projector.projectLiveSnapshot(
          conversationId,
          connectionId,
          canonical,
          frame.highestSeq
        )
        setConversation(conversationId, next, prev)
        return
      }

      try {
        let next = projector.applyLiveTranscriptEvents(prev, frame.applyEvents)
        if (next.lastAppliedSeq !== frame.highestSeq) {
          next = {
            ...next,
            lastAppliedSeq: frame.highestSeq,
            connectionId,
          }
        } else if (next.connectionId !== connectionId) {
          next = { ...next, connectionId }
        }

        // No material projection change → skip notification.
        if (
          next.segmentIds === prev.segmentIds &&
          next.segments === prev.segments &&
          next.tools === prev.tools &&
          next.status === prev.status &&
          next.lastAppliedSeq === prev.lastAppliedSeq &&
          next.connectionId === prev.connectionId
        ) {
          return
        }

        setConversation(conversationId, next, prev)
      } catch (err) {
        console.error(
          "[live-transcript-store] projector failed; rebuilding from canonical",
          err
        )
        rebuildCount += 1
        // Recovery: rebuild from already-committed canonical at the same cursor.
        const recovered = projector.projectLiveSnapshot(
          conversationId,
          connectionId,
          canonical,
          frame.highestSeq
        )
        setConversation(conversationId, recovered, prev)
      }
    },

    markCompleting(conversationId, messageId) {
      const prev = conversations.get(conversationId)
      if (!prev || prev.messageId !== messageId) return
      if (prev.status === "completing") return
      const next: LiveTranscriptSnapshot = { ...prev, status: "completing" }
      setConversation(conversationId, next, prev)
    },

    removeIfMessage(conversationId, messageId) {
      const prev = conversations.get(conversationId)
      if (!prev || prev.messageId !== messageId) return
      const prevGroups = toolGroupsByConversation.get(conversationId)
      conversations.delete(conversationId)
      toolGroupsByConversation.delete(conversationId)
      toolGroupIdsByConversation.delete(conversationId)
      notifyConversation(conversationId)
      for (const id of prev.segmentIds) {
        notifySegment(conversationId, id)
      }
      for (const toolId of prev.tools.keys()) {
        notifyTool(conversationId, toolId)
      }
      if (prevGroups) {
        for (const groupId of prevGroups.keys()) {
          notifyToolGroup(conversationId, groupId)
        }
      }
    },

    remove(conversationId) {
      const prev = conversations.get(conversationId)
      if (!prev) return
      const prevGroups = toolGroupsByConversation.get(conversationId)
      conversations.delete(conversationId)
      toolGroupsByConversation.delete(conversationId)
      toolGroupIdsByConversation.delete(conversationId)
      notifyConversation(conversationId)
      for (const id of prev.segmentIds) {
        notifySegment(conversationId, id)
      }
      for (const toolId of prev.tools.keys()) {
        notifyTool(conversationId, toolId)
      }
      if (prevGroups) {
        for (const groupId of prevGroups.keys()) {
          notifyToolGroup(conversationId, groupId)
        }
      }
    },

    migrate(fromConversationId, toConversationId) {
      if (fromConversationId === toConversationId) return
      const from = conversations.get(fromConversationId)
      if (!from) return
      conversations.delete(fromConversationId)
      const fromGroups = toolGroupsByConversation.get(fromConversationId)
      const fromGroupIds = toolGroupIdsByConversation.get(fromConversationId)
      toolGroupsByConversation.delete(fromConversationId)
      toolGroupIdsByConversation.delete(fromConversationId)
      const next: LiveTranscriptSnapshot = {
        ...from,
        conversationId: toConversationId,
      }
      conversations.set(toConversationId, next)
      if (fromGroups) {
        toolGroupsByConversation.set(toConversationId, fromGroups)
        toolGroupIdsByConversation.set(
          toConversationId,
          fromGroupIds ?? Object.freeze([...fromGroups.keys()])
        )
      } else {
        const computed = buildLiveToolGroupSummaries(next)
        toolGroupsByConversation.set(toConversationId, computed)
        toolGroupIdsByConversation.set(
          toConversationId,
          computed.size === 0
            ? STABLE_EMPTY_IDS
            : Object.freeze([...computed.keys()])
        )
      }
      notifyConversation(fromConversationId)
      notifyConversation(toConversationId)
      for (const id of next.segmentIds) {
        notifySegment(toConversationId, id)
      }
      for (const toolId of next.tools.keys()) {
        notifyTool(toConversationId, toolId)
      }
      const groups = toolGroupsByConversation.get(toConversationId)
      if (groups) {
        for (const groupId of groups.keys()) {
          notifyToolGroup(toConversationId, groupId)
        }
      }
    },

    getDebugStats() {
      let segments = 0
      let tools = 0
      let toolGroups = 0
      for (const snap of conversations.values()) {
        segments += snap.segments.size
        tools += snap.tools.size
      }
      for (const groups of toolGroupsByConversation.values()) {
        toolGroups += groups.size
      }
      return {
        rebuildCount,
        conversations: conversations.size,
        segments,
        tools,
        toolGroups,
      }
    },

    reset() {
      if (conversations.size === 0) {
        rebuildCount = 0
        toolGroupsByConversation.clear()
        toolGroupIdsByConversation.clear()
        clearCompletedStreamingPartitions()
        return
      }
      const ids = [...conversations.keys()]
      conversations.clear()
      toolGroupsByConversation.clear()
      toolGroupIdsByConversation.clear()
      rebuildCount = 0
      clearCompletedStreamingPartitions()
      for (const id of ids) {
        notifyConversation(id)
      }
    },
  }

  return api
}

/** Process-local singleton used by UI and connection sinks. */
export const liveTranscriptStore: LiveTranscriptStoreApi =
  createLiveTranscriptStore()

registerBackendScopedStoreReset(() => {
  liveTranscriptStore.reset()
  // Clear completed Markdown + highlight caches (not active live state —
  // reset() already emptied conversations).
  resetStreamingPerformanceCaches()
})

/**
 * Build a frame sink bound to a runtime conversation + connection.
 * Connection id on the snapshot is refreshed from each accepted frame.
 */
export function createLiveTranscriptFrameSink(
  conversationId: number,
  connectionId: string,
  store: LiveTranscriptStoreApi = liveTranscriptStore
): LiveTranscriptFrameSink {
  return {
    rebuild(canonical, lastAppliedSeq) {
      store.rebuild(conversationId, connectionId, canonical, lastAppliedSeq)
    },
    publish(frame, canonical) {
      store.publish(conversationId, frame, canonical)
      const hasTurnComplete = frame.applyEvents.some(
        (event) => event.type === "turn_complete"
      )
      if (hasTurnComplete) {
        store.markCompleting(conversationId, canonical.id)
      }
    },
    markCompleting(messageId) {
      store.markCompleting(conversationId, messageId)
    },
    clear(messageId) {
      store.removeIfMessage(conversationId, messageId)
    },
  }
}

/** True when a live projection snapshot exists for this conversation. */
export function useHasLiveTranscript(conversationId: number | null): boolean {
  const subscribe = useCallback(
    (onStoreChange: () => void) => {
      if (conversationId == null) return () => {}
      return liveTranscriptStore.subscribeConversation(
        conversationId,
        onStoreChange
      )
    },
    [conversationId]
  )
  const getSnapshot = useCallback(() => {
    if (conversationId == null) return false
    return liveTranscriptStore.getConversation(conversationId) != null
  }, [conversationId])
  return useSyncExternalStore(subscribe, getSnapshot, () => false)
}

/** Stable hooks for Task 11 footer rendering. */
export function useLiveTranscriptConversation(
  conversationId: number | null
): LiveTranscriptSnapshot | null {
  const subscribe = useCallback(
    (onStoreChange: () => void) => {
      if (conversationId == null) return () => {}
      return liveTranscriptStore.subscribeConversation(
        conversationId,
        onStoreChange
      )
    },
    [conversationId]
  )
  const getSnapshot = useCallback(() => {
    if (conversationId == null) return STABLE_NULL
    return liveTranscriptStore.getConversation(conversationId)
  }, [conversationId])
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot)
}

export function useLiveTranscriptSegment(
  conversationId: number | null,
  segmentId: string | null
): LiveTranscriptSegment | null {
  const subscribe = useCallback(
    (onStoreChange: () => void) => {
      if (conversationId == null || segmentId == null) return () => {}
      return liveTranscriptStore.subscribeSegment(
        conversationId,
        segmentId,
        onStoreChange
      )
    },
    [conversationId, segmentId]
  )
  const getSnapshot = useCallback(() => {
    if (conversationId == null || segmentId == null) return STABLE_NULL
    return liveTranscriptStore.getSegment(conversationId, segmentId)
  }, [conversationId, segmentId])
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot)
}

export function useLiveTranscriptTool(
  conversationId: number | null,
  toolCallId: string | null
): ToolCallInfo | null {
  const subscribe = useCallback(
    (onStoreChange: () => void) => {
      if (conversationId == null || toolCallId == null) return () => {}
      return liveTranscriptStore.subscribeTool(
        conversationId,
        toolCallId,
        onStoreChange
      )
    },
    [conversationId, toolCallId]
  )
  const getSnapshot = useCallback(() => {
    if (conversationId == null || toolCallId == null) return STABLE_NULL
    return liveTranscriptStore.getTool(conversationId, toolCallId)
  }, [conversationId, toolCallId])
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot)
}

export function useLiveTranscriptToolGroup(
  conversationId: number | null,
  groupId: string | null
): LiveToolGroupSummary | null {
  const subscribe = useCallback(
    (onStoreChange: () => void) => {
      if (conversationId == null || groupId == null) return () => {}
      return liveTranscriptStore.subscribeToolGroup(
        conversationId,
        groupId,
        onStoreChange
      )
    },
    [conversationId, groupId]
  )
  const getSnapshot = useCallback(() => {
    if (conversationId == null || groupId == null) return STABLE_NULL
    return liveTranscriptStore.getToolGroup(conversationId, groupId)
  }, [conversationId, groupId])
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot)
}

/** Stable list of structural live tool-group ids for the footer layout. */
export function useLiveTranscriptToolGroupIds(
  conversationId: number | null
): readonly string[] {
  const subscribe = useCallback(
    (onStoreChange: () => void) => {
      if (conversationId == null) return () => {}
      return liveTranscriptStore.subscribeConversation(
        conversationId,
        onStoreChange
      )
    },
    [conversationId]
  )
  const getSnapshot = useCallback(() => {
    if (conversationId == null) return STABLE_EMPTY_IDS
    return liveTranscriptStore.getToolGroupIds(conversationId)
  }, [conversationId])
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot)
}

export function useLiveTranscriptSegmentIds(
  conversationId: number | null
): readonly string[] {
  const subscribe = useCallback(
    (onStoreChange: () => void) => {
      if (conversationId == null) return () => {}
      return liveTranscriptStore.subscribeConversation(
        conversationId,
        onStoreChange
      )
    },
    [conversationId]
  )
  const getSnapshot = useCallback(() => {
    if (conversationId == null) return STABLE_EMPTY_IDS
    return (
      liveTranscriptStore.getConversation(conversationId)?.segmentIds ??
      STABLE_EMPTY_IDS
    )
  }, [conversationId])
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot)
}

/** @internal test helper */
export function __resetLiveTranscriptStoreForTests(): void {
  if (process.env.NODE_ENV !== "test") return
  liveTranscriptStore.reset()
}
