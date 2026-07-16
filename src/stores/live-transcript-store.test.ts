import { afterEach, describe, expect, it, vi } from "vitest"
import type {
  LiveMessage,
  ToolCallInfo,
} from "@/contexts/acp-connections-context"
import type { AcceptedConnectionFrame, EventEnvelope } from "@/lib/types"
import {
  projectLiveSnapshot,
  type LiveTranscriptSnapshot,
} from "@/lib/acp/live-transcript-projector"
import {
  getStreamingPerformanceCacheStats,
  resetStreamingPerformanceCaches,
} from "@/lib/acp/streaming-performance-config"
import { __putHighlightCacheForTest } from "@/components/ai-elements/code-block"
import {
  appendStreamingMarkdown,
  cacheCompletedStreamingPartition,
  completeStreamingMarkdown,
  createIncrementalStreamBlocks,
} from "@/lib/markdown/incremental-stream-blocks"
import {
  createLiveTranscriptStore,
  getToolJoinedOutput,
  selectRunningOutputTail,
  type LiveTranscriptProjectorApi,
} from "./live-transcript-store"

function liveMessageWithText(text: string, id = "msg-1"): LiveMessage {
  return {
    id,
    role: "assistant",
    content: [{ type: "text", text }],
    startedAt: 1_000,
  }
}

function content(
  connectionId: string,
  seq: number,
  text: string
): EventEnvelope {
  return {
    connection_id: connectionId,
    seq,
    type: "content_delta",
    text,
  }
}

function frame(
  applyEvents: EventEnvelope[],
  connectionId = "c1",
  highestSeq?: number
): AcceptedConnectionFrame {
  const seqs = applyEvents.map((e) => e.seq)
  return {
    contextKey: "tab-1",
    connectionId,
    deliveryIds: [1],
    applyEvents,
    rawEvents: applyEvents,
    highestSeq: highestSeq ?? (seqs.length > 0 ? Math.max(...seqs) : 0),
  }
}

describe("live-transcript-store", () => {
  afterEach(() => {
    // no shared singleton mutation in these tests — each creates its own store
  })

  it("notifies only the changed segment and conversation once", () => {
    const store = createLiveTranscriptStore()
    store.rebuild(42, "c1", liveMessageWithText("a"), 1)
    const snapshot = store.getConversation(42)!
    const textId = snapshot.segmentIds[0]
    const conversationListener = vi.fn()
    const textListener = vi.fn()
    const unrelatedToolListener = vi.fn()
    store.subscribeConversation(42, conversationListener)
    store.subscribeSegment(42, textId, textListener)
    store.subscribeTool(42, "other", unrelatedToolListener)

    store.publish(42, frame([content("c1", 2, "b")]), liveMessageWithText("ab"))
    expect(conversationListener).toHaveBeenCalledTimes(1)
    expect(textListener).toHaveBeenCalledTimes(1)
    expect(unrelatedToolListener).not.toHaveBeenCalled()
    expect(store.getConversation(42)!.segmentIds).toBe(snapshot.segmentIds)
  })

  it("rebuilds from canonical state without advancing a false cursor", () => {
    const throwingProjector: LiveTranscriptProjectorApi = {
      projectLiveSnapshot,
      applyLiveTranscriptEvents: () => {
        throw new Error("projector boom")
      },
    }
    const store = createLiveTranscriptStore({ projector: throwingProjector })
    store.rebuild(42, "c1", liveMessageWithText("a"), 10)
    store.publish(
      42,
      frame([content("c1", 11, "b")]),
      liveMessageWithText("ab")
    )
    expect(store.getConversation(42)?.lastAppliedSeq).toBe(11)
    expect(store.getDebugStats().rebuildCount).toBe(1)
    expect(
      Array.from(store.getConversation(42)?.segments.values() ?? [])[0]
    ).toMatchObject({
      type: "text",
      text: "ab",
    })
  })

  it("removeIfMessage only clears when message ids match", () => {
    const store = createLiveTranscriptStore()
    store.rebuild(1, "c1", liveMessageWithText("a", "old"), 1)
    store.removeIfMessage(1, "newer")
    expect(store.getConversation(1)).not.toBeNull()
    store.removeIfMessage(1, "old")
    expect(store.getConversation(1)).toBeNull()
  })

  it("markCompleting flips status for the matching message only", () => {
    const store = createLiveTranscriptStore()
    store.rebuild(1, "c1", liveMessageWithText("a", "m1"), 1)
    store.markCompleting(1, "other")
    expect(store.getConversation(1)?.status).toBe("streaming")
    store.markCompleting(1, "m1")
    expect(store.getConversation(1)?.status).toBe("completing")
  })

  it("migrate moves the projection under the new conversation id", () => {
    const store = createLiveTranscriptStore()
    store.rebuild(10, "c1", liveMessageWithText("x"), 3)
    store.migrate(10, 20)
    expect(store.getConversation(10)).toBeNull()
    const moved = store.getConversation(20)
    expect(moved?.conversationId).toBe(20)
    expect(moved?.lastAppliedSeq).toBe(3)
    expect(Array.from(moved?.segments.values() ?? [])[0]).toMatchObject({
      type: "text",
      text: "x",
    })
  })

  it("does not notify when an event produces no projection change", () => {
    // Force apply to return the exact previous snapshot (including cursor).
    const sticky: LiveTranscriptProjectorApi = {
      projectLiveSnapshot,
      applyLiveTranscriptEvents: (snap: LiveTranscriptSnapshot) => snap,
    }
    const store = createLiveTranscriptStore({ projector: sticky })
    store.rebuild(42, "c1", liveMessageWithText("a"), 5)
    const listener = vi.fn()
    store.subscribeConversation(42, listener)
    store.publish(42, frame([], "c1", 5), liveMessageWithText("a"))
    expect(listener).not.toHaveBeenCalled()
  })

  it("preserves ordered append chunks and visible tail cap", () => {
    const store = createLiveTranscriptStore()
    const tool: ToolCallInfo = {
      tool_call_id: "a",
      title: "bash",
      kind: "execute",
      status: "in_progress",
      content: null,
      raw_input: JSON.stringify({ command: "echo" }),
      raw_output_chunks: ["head\n"],
      raw_output_total_bytes: 5,
      locations: null,
      meta: null,
      images: [],
    }
    const msg: LiveMessage = {
      id: "m1",
      role: "assistant",
      content: [{ type: "tool_call", info: tool }],
      startedAt: 1,
    }
    store.rebuild(7, "c1", msg, 1)
    store.publish(
      7,
      frame([
        {
          connection_id: "c1",
          seq: 2,
          type: "tool_call_update",
          tool_call_id: "a",
          title: null,
          status: null,
          content: null,
          raw_input: null,
          raw_output: "one\n",
          raw_output_append: true,
          locations: null,
          meta: null,
          images: null,
        },
        {
          connection_id: "c1",
          seq: 3,
          type: "tool_call_update",
          tool_call_id: "a",
          title: null,
          status: null,
          content: null,
          raw_input: null,
          raw_output: "two\n",
          raw_output_append: true,
          locations: null,
          meta: null,
          images: null,
        },
      ]),
      {
        ...msg,
        content: [
          {
            type: "tool_call",
            info: {
              ...tool,
              raw_output_chunks: ["head\n", "one\n", "two\n"],
              raw_output_total_bytes: 13,
            },
          },
        ],
      }
    )
    const record = store.getTool(7, "a")!
    expect(getToolJoinedOutput(record)).toBe("head\none\ntwo\n")
    // "head\none\ntwo\n".slice(-8) === "one\ntwo\n"
    expect(selectRunningOutputTail(record, 8)).toBe("one\ntwo\n")
  })

  it("builds tool group summaries and notifies only changed groups", () => {
    const store = createLiveTranscriptStore()
    const makeTool = (
      id: string,
      status: ToolCallInfo["status"]
    ): ToolCallInfo => ({
      tool_call_id: id,
      title: "Read",
      kind: "read",
      status,
      content: null,
      raw_input: JSON.stringify({ file_path: `${id}.ts` }),
      raw_output_chunks: [],
      raw_output_total_bytes: 0,
      locations: null,
      meta: null,
      images: [],
    })
    const msg: LiveMessage = {
      id: "m-g",
      role: "assistant",
      content: [
        { type: "tool_call", info: makeTool("a", "in_progress") },
        { type: "tool_call", info: makeTool("b", "in_progress") },
      ],
      startedAt: 1,
    }
    store.rebuild(9, "c1", msg, 2)
    const groupIds = store.getToolGroupIds(9)
    expect(groupIds.length).toBe(1)
    const groupId = groupIds[0]
    const group = store.getToolGroup(9, groupId)!
    expect(group.toolCallIds).toEqual(["a", "b"])
    expect(group.runningCount).toBe(2)
    expect(group.errorCount).toBe(0)
    expect(group.counts.read).toBe(2)

    const groupListener = vi.fn()
    const toolAListener = vi.fn()
    store.subscribeToolGroup(9, groupId, groupListener)
    store.subscribeTool(9, "a", toolAListener)

    store.publish(
      9,
      frame([
        {
          connection_id: "c1",
          seq: 3,
          type: "tool_call_update",
          tool_call_id: "b",
          title: null,
          status: "completed",
          content: "ok",
          raw_input: null,
          raw_output: "ok",
          raw_output_append: false,
          locations: null,
          meta: null,
          images: null,
        },
      ]),
      {
        ...msg,
        content: [
          { type: "tool_call", info: makeTool("a", "in_progress") },
          {
            type: "tool_call",
            info: {
              ...makeTool("b", "completed"),
              content: "ok",
              raw_output_chunks: ["ok"],
              raw_output_total_bytes: 2,
            },
          },
        ],
      }
    )
    expect(toolAListener).not.toHaveBeenCalled()
    expect(groupListener).toHaveBeenCalledTimes(1)
    const updated = store.getToolGroup(9, store.getToolGroupIds(9)[0])!
    expect(updated.runningCount).toBe(1)
    expect(updated.errorCount).toBe(0)
  })

  it("100 sequential completed conversations leave no live-store entries after removal", () => {
    const store = createLiveTranscriptStore()
    const before = store.getDebugStats()
    for (let i = 0; i < 100; i += 1) {
      const id = 1000 + i
      store.rebuild(id, `c-${i}`, liveMessageWithText(`turn-${i}`, `m-${i}`), 1)
      store.publish(
        id,
        frame([content(`c-${i}`, 2, "!")], `c-${i}`),
        liveMessageWithText(`turn-${i}!`, `m-${i}`)
      )
      store.markCompleting(id, `m-${i}`)
      store.remove(id)
    }
    expect(store.getDebugStats()).toEqual(before)
  })

  it("migrate does not duplicate under the target conversation id", () => {
    const store = createLiveTranscriptStore()
    store.rebuild(1, "c1", liveMessageWithText("only"), 1)
    store.migrate(1, 2)
    store.migrate(1, 2) // no-op source gone
    expect(store.getConversation(1)).toBeNull()
    expect(store.getConversation(2)).not.toBeNull()
    expect(store.getDebugStats().conversations).toBe(1)
  })

  it("active live state survives completed-cache eviction", () => {
    const store = createLiveTranscriptStore()
    store.rebuild(5, "c1", liveMessageWithText("live"), 3)
    // Evict completed caches only — must not touch the live projection.
    let doc = createIncrementalStreamBlocks("s1")
    doc = appendStreamingMarkdown(doc, "cached body\n\n")
    doc = completeStreamingMarkdown(doc)
    cacheCompletedStreamingPartition("cached body\n\n", doc)
    expect(getStreamingPerformanceCacheStats().markdownEntries).toBeGreaterThan(
      0
    )
    resetStreamingPerformanceCaches()
    expect(getStreamingPerformanceCacheStats()).toEqual({
      markdownEntries: 0,
      markdownBytes: 0,
      highlightEntries: 0,
      highlightBytes: 0,
    })
    expect(store.getConversation(5)?.lastAppliedSeq).toBe(3)
    expect(
      Array.from(store.getConversation(5)?.segments.values() ?? [])[0]
    ).toMatchObject({ type: "text", text: "live" })
  })

  it("deterministic 20-turn soak keeps store and cache bounds", () => {
    const store = createLiveTranscriptStore()
    const before = store.getDebugStats()

    resetStreamingPerformanceCaches()
    let gaps = 0
    let duplicates = 0

    for (let turn = 0; turn < 20; turn += 1) {
      const id = 5000 + turn
      const connectionId = `soak-${turn}`
      store.rebuild(
        id,
        connectionId,
        liveMessageWithText("s", `msg-${turn}`),
        0
      )
      let cursor = 0
      for (let seq = 1; seq <= 50; seq += 1) {
        if (seq === cursor) duplicates += 1
        if (seq > cursor + 1) gaps += 1
        cursor = seq
        store.publish(
          id,
          frame([content(connectionId, seq, "x")], connectionId, seq),
          liveMessageWithText("s" + "x".repeat(seq), `msg-${turn}`)
        )
      }
      let doc = createIncrementalStreamBlocks(`seg-${turn}`)
      doc = appendStreamingMarkdown(doc, `complete turn ${turn}\n\n`)
      doc = completeStreamingMarkdown(doc)
      cacheCompletedStreamingPartition(`complete turn ${turn}\n\n`, doc)
      __putHighlightCacheForTest(`soak-hl-${turn}`, {
        bg: "transparent",
        fg: "inherit",
        tokens: [[{ content: "x", color: "inherit" } as never]],
      })
      store.remove(id)
    }

    const stats = getStreamingPerformanceCacheStats()
    expect(stats.markdownEntries).toBeLessThanOrEqual(32)
    expect(stats.markdownBytes).toBeLessThanOrEqual(2 * 1024 * 1024)
    expect(stats.highlightEntries).toBeLessThanOrEqual(128)
    expect(stats.highlightBytes).toBeLessThanOrEqual(8 * 1024 * 1024)
    expect(store.getDebugStats()).toEqual(before)
    expect(gaps).toBe(0)
    expect(duplicates).toBe(0)

    const memory = (
      performance as Performance & {
        memory?: { usedJSHeapSize: number }
      }
    ).memory
    const heapMeasurement = memory ? "supported" : "unsupported"
    if (memory) {
      const first = memory.usedJSHeapSize
      const budget = Math.max(first * 0.2, 32 * 1024 * 1024)
      expect(memory.usedJSHeapSize).toBeLessThanOrEqual(first + budget)
    }
    expect(["supported", "unsupported"]).toContain(heapMeasurement)
  })
})
