/**
 * Pure live-transcript projector.
 *
 * Builds and incrementally updates a UI-only segment projection from the
 * canonical connection-layer `LiveMessage` + accepted ACP envelopes. Canonical
 * `LiveMessage` remains the authority for reconnect, snapshot, completion, and
 * persistence; this module never writes back into connection state.
 */

import type {
  LiveContentBlock,
  LiveMessage,
  ToolCallInfo,
  ToolCallImage,
  ToolCallMeta,
} from "@/contexts/acp-connections-context"
import type {
  AgentType,
  DelegationActivityView,
  EventEnvelope,
  PlanEntryInfo,
} from "@/lib/types"
import {
  appendStreamingMarkdown,
  createIncrementalStreamBlocks,
  sealStreamingMarkdownBoundary,
  type IncrementalStreamBlocks,
} from "@/lib/markdown/incremental-stream-blocks"
import { deriveNativeActivitiesFromToolCalls } from "@/lib/delegation-activity"

/** Cross-task live segment (UI projection only). */
export type LiveTranscriptSegment =
  | {
      id: string
      type: "text"
      text: string
      document: IncrementalStreamBlocks
    }
  | { id: string; type: "thinking"; text: string }
  | { id: string; type: "tool"; toolCallId: string }
  | { id: string; type: "plan"; entries: PlanEntryInfo[] }
  | { id: string; type: "generated-image"; toolCallId: string }

function createTextSegment(
  id: string,
  text: string
): Extract<LiveTranscriptSegment, { type: "text" }> {
  return {
    id,
    type: "text",
    text,
    document: appendStreamingMarkdown(createIncrementalStreamBlocks(id), text),
  }
}

/** Cross-task live projection snapshot. */
export interface LiveTranscriptSnapshot {
  conversationId: number
  connectionId: string
  messageId: string
  startedAt: number
  status: "streaming" | "completing"
  segmentIds: readonly string[]
  segments: ReadonlyMap<string, LiveTranscriptSegment>
  tools: ReadonlyMap<string, ToolCallInfo>
  lastAppliedSeq: number
}

/** Max live tool raw-output retention (matches connection reducer). */
const MAX_LIVE_TOOL_RAW_OUTPUT_CHARS = 200_000

function segmentId(messageId: string, ordinal: number, kind: string): string {
  return `${messageId}:${kind}:${ordinal}`
}

function isImageBearingTool(info: ToolCallInfo): boolean {
  if ((info.images?.length ?? 0) > 0) return true
  const title = (info.title ?? "").trim()
  return (
    title === "Image generation" || title.toLowerCase() === "image generation"
  )
}

function toolSegmentType(info: ToolCallInfo): "tool" | "generated-image" {
  return isImageBearingTool(info) ? "generated-image" : "tool"
}

function emptyToolInfo(
  toolCallId: string,
  overrides: Partial<ToolCallInfo> = {}
): ToolCallInfo {
  return {
    tool_call_id: toolCallId,
    title: overrides.title ?? "",
    kind: overrides.kind ?? "other",
    status: overrides.status ?? "pending",
    content: overrides.content ?? null,
    raw_input: overrides.raw_input ?? null,
    raw_output_chunks: overrides.raw_output_chunks ?? [],
    raw_output_total_bytes: overrides.raw_output_total_bytes ?? 0,
    locations: overrides.locations ?? null,
    meta: (overrides.meta ?? null) as ToolCallMeta,
    images: overrides.images ?? [],
  }
}

function appendRawOutput(
  existing: ToolCallInfo,
  rawOutput: string,
  append: boolean
): Pick<ToolCallInfo, "raw_output_chunks" | "raw_output_total_bytes"> {
  if (!append) {
    return {
      raw_output_chunks: [rawOutput],
      raw_output_total_bytes: rawOutput.length,
    }
  }
  let newChunks = [...existing.raw_output_chunks, rawOutput]
  let newTotalBytes = existing.raw_output_total_bytes + rawOutput.length
  if (newTotalBytes > MAX_LIVE_TOOL_RAW_OUTPUT_CHARS && newChunks.length > 1) {
    let evictCount = 0
    let evictedBytes = 0
    while (
      evictCount < newChunks.length - 1 &&
      newTotalBytes - evictedBytes > MAX_LIVE_TOOL_RAW_OUTPUT_CHARS
    ) {
      evictedBytes += newChunks[evictCount].length
      evictCount++
    }
    if (evictCount > 0) {
      newChunks = newChunks.slice(evictCount)
      newTotalBytes -= evictedBytes
    }
  }
  return {
    raw_output_chunks: newChunks,
    raw_output_total_bytes: newTotalBytes,
  }
}

function countKindOrdinals(
  segments: ReadonlyMap<string, LiveTranscriptSegment>
): { text: number; thinking: number; tool: number } {
  let text = 0
  let thinking = 0
  let tool = 0
  for (const segment of segments.values()) {
    if (segment.type === "text") text++
    else if (segment.type === "thinking") thinking++
    else if (segment.type === "tool" || segment.type === "generated-image")
      tool++
  }
  return { text, thinking, tool }
}

/**
 * Project a full snapshot from the already-committed canonical live message.
 * Walks content once; segment IDs come from per-kind ordinals, never renumbered
 * array indexes. Plan always uses stable `:plan:0` and is placed at its
 * content index (not forced to the end). `applyPlanUpdate` is what moves the
 * plan to the end on incremental plan_update events.
 */
export function projectLiveSnapshot(
  conversationId: number,
  connectionId: string,
  canonical: LiveMessage,
  lastAppliedSeq: number
): LiveTranscriptSnapshot {
  const segmentIds: string[] = []
  const segments = new Map<string, LiveTranscriptSegment>()
  const tools = new Map<string, ToolCallInfo>()
  let textOrdinal = 0
  let thinkingOrdinal = 0
  let toolOrdinal = 0

  for (const block of canonical.content) {
    switch (block.type) {
      case "text": {
        const id = segmentId(canonical.id, textOrdinal++, "text")
        segmentIds.push(id)
        segments.set(id, createTextSegment(id, block.text))
        break
      }
      case "thinking": {
        const id = segmentId(canonical.id, thinkingOrdinal++, "thinking")
        segmentIds.push(id)
        segments.set(id, { id, type: "thinking", text: block.text })
        break
      }
      case "tool_call": {
        const info = block.info
        tools.set(info.tool_call_id, info)
        // Stable tool ordinal ID even when classified as generated-image so
        // snapshot rebuild and incremental append share identity.
        const id = segmentId(canonical.id, toolOrdinal++, "tool")
        const type = toolSegmentType(info)
        segmentIds.push(id)
        segments.set(
          id,
          type === "generated-image"
            ? { id, type: "generated-image", toolCallId: info.tool_call_id }
            : { id, type: "tool", toolCallId: info.tool_call_id }
        )
        break
      }
      case "plan": {
        // Stable plan:0 at the content index. Multiple plan blocks collapse to
        // one id; later entries replace earlier ones in place order.
        const id = segmentId(canonical.id, 0, "plan")
        const existing = segmentIds.indexOf(id)
        if (existing >= 0) segmentIds.splice(existing, 1)
        segmentIds.push(id)
        segments.set(id, { id, type: "plan", entries: block.entries })
        break
      }
    }
  }

  return {
    conversationId,
    connectionId,
    messageId: canonical.id,
    startedAt: canonical.startedAt,
    status: "streaming",
    segmentIds,
    segments,
    tools,
    lastAppliedSeq,
  }
}

function cloneSnapshot(
  snapshot: LiveTranscriptSnapshot,
  patch: Partial<LiveTranscriptSnapshot> & {
    segmentIds?: readonly string[]
    segments?: ReadonlyMap<string, LiveTranscriptSegment>
    tools?: ReadonlyMap<string, ToolCallInfo>
  }
): LiveTranscriptSnapshot {
  return {
    conversationId: patch.conversationId ?? snapshot.conversationId,
    connectionId: patch.connectionId ?? snapshot.connectionId,
    messageId: patch.messageId ?? snapshot.messageId,
    startedAt: patch.startedAt ?? snapshot.startedAt,
    status: patch.status ?? snapshot.status,
    segmentIds: patch.segmentIds ?? snapshot.segmentIds,
    segments: patch.segments ?? snapshot.segments,
    tools: patch.tools ?? snapshot.tools,
    lastAppliedSeq: patch.lastAppliedSeq ?? snapshot.lastAppliedSeq,
  }
}

function lastSegment(
  snapshot: LiveTranscriptSnapshot
): LiveTranscriptSegment | undefined {
  const lastId = snapshot.segmentIds[snapshot.segmentIds.length - 1]
  return lastId ? snapshot.segments.get(lastId) : undefined
}

/** Seal the trailing text segment before a non-text block is appended. */
function sealTrailingText(
  snapshot: LiveTranscriptSnapshot
): LiveTranscriptSnapshot {
  const last = lastSegment(snapshot)
  if (last?.type !== "text") return snapshot
  const sealedDoc = sealStreamingMarkdownBoundary(last.document)
  // Avoid churning segment identity when the boundary seal is a no-op.
  if (
    sealedDoc.sealed === last.document.sealed &&
    sealedDoc.tail === last.document.tail &&
    sealedDoc.valid === last.document.valid &&
    sealedDoc.nextBlockIndex === last.document.nextBlockIndex
  ) {
    return snapshot
  }
  const segments = new Map(snapshot.segments)
  segments.set(last.id, { ...last, document: sealedDoc })
  return cloneSnapshot(snapshot, { segments })
}

function applyContentDelta(
  snapshot: LiveTranscriptSnapshot,
  text: string
): LiveTranscriptSnapshot {
  if (text.length === 0) return snapshot
  const last = lastSegment(snapshot)
  if (last?.type === "text") {
    const nextSeg: LiveTranscriptSegment = {
      ...last,
      text: last.text + text,
      document: appendStreamingMarkdown(last.document, text),
    }
    const segments = new Map(snapshot.segments)
    segments.set(last.id, nextSeg)
    return cloneSnapshot(snapshot, { segments })
  }
  const ordinals = countKindOrdinals(snapshot.segments)
  const id = segmentId(snapshot.messageId, ordinals.text, "text")
  const segments = new Map(snapshot.segments)
  segments.set(id, createTextSegment(id, text))
  return cloneSnapshot(snapshot, {
    segmentIds: [...snapshot.segmentIds, id],
    segments,
  })
}

function applyThinking(
  snapshot: LiveTranscriptSnapshot,
  text: string
): LiveTranscriptSnapshot {
  const last = lastSegment(snapshot)
  if (text.length === 0 && last?.type === "thinking") {
    return snapshot
  }
  if (last?.type === "thinking") {
    const nextSeg: LiveTranscriptSegment = {
      ...last,
      text: last.text + text,
    }
    const segments = new Map(snapshot.segments)
    segments.set(last.id, nextSeg)
    return cloneSnapshot(snapshot, { segments })
  }
  // Seal preceding text at the thinking boundary so completed blocks upgrade.
  const base = sealTrailingText(snapshot)
  const ordinals = countKindOrdinals(base.segments)
  const id = segmentId(base.messageId, ordinals.thinking, "thinking")
  const segments = new Map(base.segments)
  segments.set(id, { id, type: "thinking", text })
  return cloneSnapshot(base, {
    segmentIds: [...base.segmentIds, id],
    segments,
  })
}

function findToolSegmentId(
  snapshot: LiveTranscriptSnapshot,
  toolCallId: string
): string | null {
  for (const [id, segment] of snapshot.segments) {
    if (
      (segment.type === "tool" || segment.type === "generated-image") &&
      segment.toolCallId === toolCallId
    ) {
      return id
    }
  }
  return null
}

function upsertTool(
  snapshot: LiveTranscriptSnapshot,
  info: ToolCallInfo,
  addSegmentIfMissing: boolean
): LiveTranscriptSnapshot {
  const tools = new Map(snapshot.tools)
  tools.set(info.tool_call_id, info)
  const existingId = findToolSegmentId(snapshot, info.tool_call_id)
  if (existingId) {
    const prev = snapshot.segments.get(existingId)
    if (!prev || (prev.type !== "tool" && prev.type !== "generated-image")) {
      return cloneSnapshot(snapshot, { tools })
    }
    const type = toolSegmentType(info)
    if (prev.type === type) {
      return cloneSnapshot(snapshot, { tools })
    }
    // Classification may flip to generated-image once images arrive; keep id.
    const segments = new Map(snapshot.segments)
    segments.set(
      existingId,
      type === "generated-image"
        ? {
            id: existingId,
            type: "generated-image",
            toolCallId: info.tool_call_id,
          }
        : { id: existingId, type: "tool", toolCallId: info.tool_call_id }
    )
    return cloneSnapshot(snapshot, { segments, tools })
  }
  if (!addSegmentIfMissing) {
    return cloneSnapshot(snapshot, { tools })
  }
  // Seal preceding text at tool / generated-image boundaries.
  const base = sealTrailingText(snapshot)
  const ordinals = countKindOrdinals(base.segments)
  const id = segmentId(base.messageId, ordinals.tool, "tool")
  const type = toolSegmentType(info)
  const segments = new Map(base.segments)
  segments.set(
    id,
    type === "generated-image"
      ? { id, type: "generated-image", toolCallId: info.tool_call_id }
      : { id, type: "tool", toolCallId: info.tool_call_id }
  )
  // Plan stays last: insert tool before trailing plan if present.
  const planId = segmentId(base.messageId, 0, "plan")
  const ids = [...base.segmentIds]
  const planIndex = ids.indexOf(planId)
  if (planIndex >= 0) {
    ids.splice(planIndex, 0, id)
  } else {
    ids.push(id)
  }
  return cloneSnapshot(base, { segmentIds: ids, segments, tools })
}

function applyToolCall(
  snapshot: LiveTranscriptSnapshot,
  event: Extract<EventEnvelope, { type: "tool_call" }>
): LiveTranscriptSnapshot {
  const existing = snapshot.tools.get(event.tool_call_id)
  const info: ToolCallInfo = existing
    ? {
        ...existing,
        title: event.title ?? existing.title,
        kind: event.kind ?? existing.kind,
        status: event.status ?? existing.status,
        content: event.content ?? existing.content,
        raw_input: event.raw_input ?? existing.raw_input,
        raw_output_chunks:
          event.raw_output !== null
            ? [event.raw_output]
            : existing.raw_output_chunks,
        raw_output_total_bytes:
          event.raw_output !== null
            ? event.raw_output.length
            : existing.raw_output_total_bytes,
        locations: event.locations ?? existing.locations,
        meta: (event.meta as ToolCallMeta) ?? existing.meta,
        images:
          event.images !== undefined && event.images !== null
            ? event.images
            : existing.images,
      }
    : emptyToolInfo(event.tool_call_id, {
        title: event.title,
        kind: event.kind,
        status: event.status,
        content: event.content,
        raw_input: event.raw_input,
        raw_output_chunks: event.raw_output !== null ? [event.raw_output] : [],
        raw_output_total_bytes: event.raw_output?.length ?? 0,
        locations: event.locations ?? null,
        meta: (event.meta as ToolCallMeta) ?? null,
        images: event.images ?? [],
      })
  return upsertTool(snapshot, info, true)
}

function applyToolCallUpdate(
  snapshot: LiveTranscriptSnapshot,
  event: Extract<EventEnvelope, { type: "tool_call_update" }>
): LiveTranscriptSnapshot {
  const existing = snapshot.tools.get(event.tool_call_id)
  let info: ToolCallInfo
  if (!existing) {
    const initialChunks = event.raw_output !== null ? [event.raw_output] : []
    info = emptyToolInfo(event.tool_call_id, {
      title: event.title ?? "tool",
      kind: "tool",
      status:
        event.status ?? (initialChunks.length > 0 ? "in_progress" : "pending"),
      content: event.content,
      raw_input: event.raw_input,
      raw_output_chunks: initialChunks,
      raw_output_total_bytes: event.raw_output?.length ?? 0,
      locations: event.locations ?? null,
      meta: (event.meta as ToolCallMeta) ?? null,
      images: event.images ?? [],
    })
  } else {
    let rawPatch: Pick<
      ToolCallInfo,
      "raw_output_chunks" | "raw_output_total_bytes"
    >
    if (event.raw_output === null) {
      rawPatch = {
        raw_output_chunks: existing.raw_output_chunks,
        raw_output_total_bytes: existing.raw_output_total_bytes,
      }
    } else if (event.raw_output_append) {
      rawPatch = appendRawOutput(existing, event.raw_output, true)
    } else {
      rawPatch = appendRawOutput(existing, event.raw_output, false)
    }
    info = {
      ...existing,
      title: event.title ?? existing.title,
      status: event.status ?? existing.status,
      content: event.content ?? existing.content,
      raw_input: event.raw_input ?? existing.raw_input,
      ...rawPatch,
      locations: event.locations ?? existing.locations,
      meta: (event.meta as ToolCallMeta) ?? existing.meta,
      // Absent images field → preserve; present (incl. []) → replace.
      images:
        event.images !== undefined && event.images !== null
          ? event.images
          : existing.images,
    }
  }
  // Add segment only if missing (mirrors reducer creating a block on update).
  return upsertTool(snapshot, info, true)
}

function applyPlanUpdate(
  snapshot: LiveTranscriptSnapshot,
  entries: PlanEntryInfo[]
): LiveTranscriptSnapshot {
  const planId = segmentId(snapshot.messageId, 0, "plan")
  const base = entries.length > 0 ? sealTrailingText(snapshot) : snapshot
  const withoutPlan = base.segmentIds.filter((id) => id !== planId)
  const segments = new Map(base.segments)
  segments.delete(planId)

  if (entries.length === 0) {
    if (withoutPlan.length === snapshot.segmentIds.length) {
      return snapshot
    }
    return cloneSnapshot(base, {
      segmentIds: withoutPlan,
      segments,
    })
  }

  const planSeg: LiveTranscriptSegment = {
    id: planId,
    type: "plan",
    entries,
  }
  segments.set(planId, planSeg)
  return cloneSnapshot(base, {
    segmentIds: [...withoutPlan, planId],
    segments,
  })
}

/**
 * Apply accepted envelopes incrementally. Structural identity of `segmentIds`
 * is preserved for pure text/thinking appends and isolated tool updates.
 * `turn_complete` and other non-content events leave the projection unchanged
 * (completion is owned by the store's `markCompleting`).
 */
export function applyLiveTranscriptEvents(
  snapshot: LiveTranscriptSnapshot,
  events: readonly EventEnvelope[]
): LiveTranscriptSnapshot {
  let current = snapshot
  let highestSeq = snapshot.lastAppliedSeq

  for (const event of events) {
    if (event.seq > highestSeq) highestSeq = event.seq
    switch (event.type) {
      case "content_delta":
        current = applyContentDelta(current, event.text)
        break
      case "thinking":
        current = applyThinking(current, event.text)
        break
      case "tool_call":
        current = applyToolCall(current, event)
        break
      case "tool_call_update":
        current = applyToolCallUpdate(current, event)
        break
      case "plan_update":
        current = applyPlanUpdate(current, event.entries)
        break
      default:
        // status_changed / turn_complete / permissions / etc. — no-op here.
        break
    }
  }

  if (highestSeq !== current.lastAppliedSeq) {
    current = cloneSnapshot(current, { lastAppliedSeq: highestSeq })
  }
  return current
}

/**
 * Rebuild a context-layer `LiveMessage` from projection segment order.
 * Used for tests and recovery parity only — not on every render.
 */
export function liveTranscriptToCanonicalMessage(
  snapshot: LiveTranscriptSnapshot
): LiveMessage {
  const content: LiveContentBlock[] = []
  for (const id of snapshot.segmentIds) {
    const segment = snapshot.segments.get(id)
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
        const info = snapshot.tools.get(segment.toolCallId)
        if (info) {
          content.push({ type: "tool_call", info })
        }
        break
      }
    }
  }
  return {
    id: snapshot.messageId,
    role: "assistant",
    content,
    startedAt: snapshot.startedAt,
  }
}

/**
 * Apply the same streaming events the connection reducer would, for parity
 * tests against incremental projection.
 */
export function applyEventsToCanonicalLiveMessage(
  snapshot: LiveMessage,
  events: readonly EventEnvelope[]
): LiveMessage {
  let message: LiveMessage = {
    ...snapshot,
    content: [...snapshot.content],
  }

  for (const event of events) {
    switch (event.type) {
      case "content_delta": {
        if (event.text.length === 0) break
        const last = message.content[message.content.length - 1]
        if (last?.type === "text") {
          message = {
            ...message,
            content: [
              ...message.content.slice(0, -1),
              { type: "text", text: last.text + event.text },
            ],
          }
        } else {
          message = {
            ...message,
            content: [...message.content, { type: "text", text: event.text }],
          }
        }
        break
      }
      case "thinking": {
        const last = message.content[message.content.length - 1]
        if (event.text.length === 0 && last?.type === "thinking") break
        if (last?.type === "thinking") {
          message = {
            ...message,
            content: [
              ...message.content.slice(0, -1),
              { type: "thinking", text: last.text + event.text },
            ],
          }
        } else {
          message = {
            ...message,
            content: [
              ...message.content,
              { type: "thinking", text: event.text },
            ],
          }
        }
        break
      }
      case "tool_call": {
        const existingIndex = message.content.findIndex(
          (b) =>
            b.type === "tool_call" && b.info.tool_call_id === event.tool_call_id
        )
        if (existingIndex !== -1) {
          const block = message.content[existingIndex]
          if (block.type !== "tool_call") break
          const info: ToolCallInfo = {
            ...block.info,
            title: event.title ?? block.info.title,
            kind: event.kind ?? block.info.kind,
            status: event.status ?? block.info.status,
            content: event.content ?? block.info.content,
            raw_input: event.raw_input ?? block.info.raw_input,
            raw_output_chunks:
              event.raw_output !== null
                ? [event.raw_output]
                : block.info.raw_output_chunks,
            raw_output_total_bytes:
              event.raw_output !== null
                ? event.raw_output.length
                : block.info.raw_output_total_bytes,
            locations: event.locations ?? block.info.locations,
            meta: (event.meta as ToolCallMeta) ?? block.info.meta,
            images:
              event.images !== undefined && event.images !== null
                ? event.images
                : block.info.images,
          }
          message = {
            ...message,
            content: [
              ...message.content.slice(0, existingIndex),
              { type: "tool_call", info },
              ...message.content.slice(existingIndex + 1),
            ],
          }
        } else {
          message = {
            ...message,
            content: [
              ...message.content,
              {
                type: "tool_call",
                info: emptyToolInfo(event.tool_call_id, {
                  title: event.title,
                  kind: event.kind,
                  status: event.status,
                  content: event.content,
                  raw_input: event.raw_input,
                  raw_output_chunks:
                    event.raw_output !== null ? [event.raw_output] : [],
                  raw_output_total_bytes: event.raw_output?.length ?? 0,
                  locations: event.locations ?? null,
                  meta: (event.meta as ToolCallMeta) ?? null,
                  images: event.images ?? [],
                }),
              },
            ],
          }
        }
        break
      }
      case "tool_call_update": {
        const existingIndex = message.content.findIndex(
          (b) =>
            b.type === "tool_call" && b.info.tool_call_id === event.tool_call_id
        )
        if (existingIndex === -1) {
          const initialChunks =
            event.raw_output !== null ? [event.raw_output] : []
          message = {
            ...message,
            content: [
              ...message.content,
              {
                type: "tool_call",
                info: emptyToolInfo(event.tool_call_id, {
                  title: event.title ?? "tool",
                  kind: "tool",
                  status:
                    event.status ??
                    (initialChunks.length > 0 ? "in_progress" : "pending"),
                  content: event.content,
                  raw_input: event.raw_input,
                  raw_output_chunks: initialChunks,
                  raw_output_total_bytes: event.raw_output?.length ?? 0,
                  locations: event.locations ?? null,
                  meta: (event.meta as ToolCallMeta) ?? null,
                  images: event.images ?? [],
                }),
              },
            ],
          }
          break
        }
        const block = message.content[existingIndex]
        if (block.type !== "tool_call") break
        let rawPatch: Pick<
          ToolCallInfo,
          "raw_output_chunks" | "raw_output_total_bytes"
        >
        if (event.raw_output === null) {
          rawPatch = {
            raw_output_chunks: block.info.raw_output_chunks,
            raw_output_total_bytes: block.info.raw_output_total_bytes,
          }
        } else if (event.raw_output_append) {
          rawPatch = appendRawOutput(block.info, event.raw_output, true)
        } else {
          rawPatch = appendRawOutput(block.info, event.raw_output, false)
        }
        const info: ToolCallInfo = {
          ...block.info,
          title: event.title ?? block.info.title,
          status: event.status ?? block.info.status,
          content: event.content ?? block.info.content,
          raw_input: event.raw_input ?? block.info.raw_input,
          ...rawPatch,
          locations: event.locations ?? block.info.locations,
          meta: (event.meta as ToolCallMeta) ?? block.info.meta,
          images:
            event.images !== undefined && event.images !== null
              ? event.images
              : block.info.images,
        }
        message = {
          ...message,
          content: [
            ...message.content.slice(0, existingIndex),
            { type: "tool_call", info },
            ...message.content.slice(existingIndex + 1),
          ],
        }
        break
      }
      case "plan_update": {
        const nonPlan = message.content.filter((b) => b.type !== "plan")
        if (event.entries.length === 0) {
          message = { ...message, content: nonPlan }
        } else {
          message = {
            ...message,
            content: [...nonPlan, { type: "plan", entries: event.entries }],
          }
        }
        break
      }
      default:
        break
    }
  }

  return message
}

export type { ToolCallImage }

/**
 * Project native read-only activity views from a live transcript snapshot.
 *
 * Pure derivation: does not mutate `snap.tools` or consume tool segments.
 * Original tool name / raw input / output / status / meta remain on the
 * snapshot for normal tool rendering.
 */
export function projectNativeActivitiesFromTranscript(
  snap: LiveTranscriptSnapshot,
  platform?: AgentType | null
): DelegationActivityView[] {
  const at = new Date(snap.startedAt).toISOString()
  const tools = [...snap.tools.values()].map((info) => {
    const output =
      info.raw_output_chunks.length > 0
        ? info.raw_output_chunks.join("")
        : (info.content ?? null)
    return {
      toolCallId: info.tool_call_id,
      toolName: (info.title ?? "").trim() || info.kind || "tool",
      input: info.raw_input ?? null,
      output,
      status: info.status ?? null,
      at,
    }
  })
  return deriveNativeActivitiesFromToolCalls(tools, platform)
}
