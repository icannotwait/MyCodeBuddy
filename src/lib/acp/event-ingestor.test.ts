import { describe, expect, it, vi } from "vitest"
import type {
  AcceptedEventFrame,
  AcpEvent,
  DesktopAcpEventBatch,
  EventEnvelope,
  SequenceGap,
} from "@/lib/types"
import { EventIngestor } from "./event-ingestor"

function batch(
  batch_id: number,
  events: EventEnvelope[]
): DesktopAcpEventBatch {
  return { batch_id, events }
}

function event(
  connection_id: string,
  seq: number,
  payload: AcpEvent
): EventEnvelope {
  return { connection_id, seq, ...payload }
}

function content(
  connectionId: string,
  seq: number,
  text: string
): EventEnvelope {
  return event(connectionId, seq, { type: "content_delta", text })
}

function thinking(
  connectionId: string,
  seq: number,
  text: string
): EventEnvelope {
  return event(connectionId, seq, { type: "thinking", text })
}

function toolAppend(
  connectionId: string,
  seq: number,
  raw_output: string
): EventEnvelope {
  return event(connectionId, seq, {
    type: "tool_call_update",
    tool_call_id: "tool-1",
    title: null,
    status: null,
    content: null,
    raw_input: null,
    raw_output,
    raw_output_append: true,
  })
}

function createIngestorHarness(
  initial: Record<string, { key: string; cursor: number }>
) {
  const connectionToKey = new Map(
    Object.entries(initial).map(([id, value]) => [id, value.key])
  )
  const cursorByKey = new Map(
    Object.values(initial).map((value) => [value.key, value.cursor])
  )
  const commits: AcceptedEventFrame[] = []
  const gaps: SequenceGap[] = []
  let scheduled: FrameRequestCallback | null = null
  const onUnmapped = vi.fn()
  const onDuplicate = vi.fn()
  let commitImpl: (frame: AcceptedEventFrame) => void = (frame) => {
    commits.push(frame)
    for (const connection of frame.connections) {
      cursorByKey.set(connection.contextKey, connection.highestSeq)
    }
  }
  const ingestor = new EventIngestor({
    resolveContextKey: (connectionId) =>
      connectionToKey.get(connectionId) ?? null,
    readCursor: (contextKey) => cursorByKey.get(contextKey) ?? 0,
    commit: (frame) => commitImpl(frame),
    onGap: (gap) => gaps.push(gap),
    onDuplicate,
    onUnmapped,
    scheduleFrame: (callback) => {
      scheduled = callback
      return 1
    },
    cancelFrame: () => {
      scheduled = null
    },
  })
  return {
    commits,
    gaps,
    onUnmapped,
    onDuplicate,
    ingestor,
    setCommit: (fn: (frame: AcceptedEventFrame) => void) => {
      commitImpl = fn
    },
    setCursor: (contextKey: string, cursor: number) => {
      cursorByKey.set(contextKey, cursor)
    },
    mapConnection: (connectionId: string, key: string) => {
      connectionToKey.set(connectionId, key)
    },
    pushBatch: (value: DesktopAcpEventBatch) => ingestor.pushBatch(value),
    runFrame: () => {
      const callback = scheduled
      scheduled = null
      callback?.(16)
    },
    isScheduled: () => scheduled !== null,
    cursor: (connectionId: string) =>
      cursorByKey.get(connectionToKey.get(connectionId) ?? "") ?? 0,
    appliedTypes: () =>
      commits.flatMap((frame) =>
        frame.connections.flatMap((connection) =>
          connection.applyEvents.map((item) => item.type)
        )
      ),
    rawSeqs: () =>
      commits.flatMap((frame) =>
        frame.rawEventsInDeliveryOrder.map((item) => item.seq)
      ),
  }
}

describe("EventIngestor", () => {
  it("deduplicates, compacts, and commits once on the next frame", () => {
    const h = createIngestorHarness({ c1: { key: "tab-1", cursor: 10 } })
    h.pushBatch(
      batch(4, [
        content("c1", 10, "old"),
        content("c1", 11, "a"),
        content("c1", 12, "b"),
      ])
    )
    h.pushBatch(batch(5, [thinking("c1", 13, "x"), thinking("c1", 14, "y")]))
    expect(h.commits).toHaveLength(0)
    h.runFrame()
    expect(h.commits).toHaveLength(1)
    expect(h.commits[0].connections[0].applyEvents).toMatchObject([
      { type: "content_delta", text: "ab", seq: 12 },
      { type: "thinking", text: "xy", seq: 14 },
    ])
    expect(
      h.commits[0].connections[0].rawEvents.map((event) => event.seq)
    ).toEqual([11, 12, 13, 14])
    expect(h.commits[0].connections[0].highestSeq).toBe(14)
    expect(h.commits[0].deliveryIds).toEqual([4, 5])
    expect(h.commits[0].connections[0].deliveryIds).toEqual([4, 5])
    // seq 10 was already applied (cursor) — reported as duplicate.
    expect(h.onDuplicate).toHaveBeenCalledWith(
      expect.objectContaining({
        connectionId: "c1",
        contextKey: "tab-1",
        seq: 10,
      })
    )
  })

  it("stops a connection at the first sequence gap", () => {
    const h = createIngestorHarness({ c1: { key: "tab-1", cursor: 20 } })
    h.pushBatch(batch(1, [content("c1", 22, "missing-21")]))
    h.runFrame()
    expect(h.commits).toHaveLength(0)
    expect(h.gaps).toEqual([
      {
        contextKey: "tab-1",
        connectionId: "c1",
        expectedSeq: 21,
        receivedSeq: 22,
      },
    ])
    expect(h.cursor("c1")).toBe(20)
  })

  it("frontend seq gap pauses only that connection then resumes contiguously", () => {
    const h = createIngestorHarness({
      cA: { key: "tab-a", cursor: 5 },
      cB: { key: "tab-b", cursor: 1 },
    })
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {})
    h.pushBatch(
      batch(1, [
        content("cA", 7, "gap-payload-must-not-log"),
        content("cB", 2, "ok-b"),
      ])
    )
    h.runFrame()
    // B still commits; A is paused for snapshot recovery.
    expect(
      h.commits.some((c) => c.connections.some((x) => x.connectionId === "cB"))
    ).toBe(true)
    expect(h.gaps).toEqual([
      {
        contextKey: "tab-a",
        connectionId: "cA",
        expectedSeq: 6,
        receivedSeq: 7,
      },
    ])
    // Content-free diagnostics only — never log event payload text.
    for (const call of errorSpy.mock.calls) {
      const joined = call.map(String).join(" ")
      expect(joined).not.toContain("gap-payload-must-not-log")
    }
    errorSpy.mockRestore()
    h.setCursor("tab-a", 7)
    h.ingestor.resumeConnection("cA", 7)
    h.pushBatch(batch(2, [content("cA", 8, "after-snapshot")]))
    h.runFrame()
    const after = h.commits
      .flatMap((c) => c.connections)
      .filter((c) => c.connectionId === "cA")
    expect(after.some((c) => c.highestSeq >= 8)).toBe(true)
  })

  it("never compacts across a tool append boundary", () => {
    const h = createIngestorHarness({ c1: { key: "tab-1", cursor: 0 } })
    h.pushBatch(
      batch(1, [
        content("c1", 1, "a"),
        toolAppend("c1", 2, "x"),
        content("c1", 3, "b"),
      ])
    )
    h.runFrame()
    expect(h.appliedTypes()).toEqual([
      "content_delta",
      "tool_call_update",
      "content_delta",
    ])
    expect(h.rawSeqs()).toEqual([1, 2, 3])
  })

  it("calls onUnmapped in original order without advancing a cursor", () => {
    const h = createIngestorHarness({ c1: { key: "tab-1", cursor: 0 } })
    h.pushBatch(
      batch(1, [
        content("unknown", 1, "x"),
        content("c1", 1, "a"),
        content("unknown", 2, "y"),
      ])
    )
    h.runFrame()
    expect(h.onUnmapped).toHaveBeenCalledTimes(2)
    expect(h.onUnmapped.mock.calls[0][0].connection_id).toBe("unknown")
    expect(h.onUnmapped.mock.calls[0][0].seq).toBe(1)
    expect(h.onUnmapped.mock.calls[1][0].seq).toBe(2)
    expect(h.commits).toHaveLength(1)
    expect(h.rawSeqs()).toEqual([1])
    expect(h.cursor("c1")).toBe(1)
  })

  it("buffers events while a connection is paused", () => {
    const h = createIngestorHarness({ c1: { key: "tab-1", cursor: 0 } })
    h.ingestor.pauseConnection("c1")
    h.pushBatch(batch(1, [content("c1", 1, "a"), content("c1", 2, "b")]))
    h.runFrame()
    expect(h.commits).toHaveLength(0)
    expect(h.cursor("c1")).toBe(0)

    h.ingestor.resumeConnection("c1", 0)
    h.runFrame()
    expect(h.commits).toHaveLength(1)
    expect(h.rawSeqs()).toEqual([1, 2])
    expect(h.cursor("c1")).toBe(2)
  })

  it("resumeConnection drops buffered duplicates then requires contiguity", () => {
    const h = createIngestorHarness({ c1: { key: "tab-1", cursor: 5 } })
    h.pushBatch(batch(1, [content("c1", 7, "gap"), content("c1", 8, "after")]))
    h.runFrame()
    expect(h.gaps).toHaveLength(1)
    expect(h.commits).toHaveLength(0)

    // Recovery snapshot advanced through seq 7; resume from there.
    h.setCursor("tab-1", 7)
    h.ingestor.resumeConnection("c1", 7)
    h.runFrame()
    expect(h.commits).toHaveLength(1)
    expect(h.rawSeqs()).toEqual([8])
    expect(h.cursor("c1")).toBe(8)
  })

  it("keeps independent cursors for interleaved connections", () => {
    const h = createIngestorHarness({
      c1: { key: "tab-1", cursor: 0 },
      c2: { key: "tab-2", cursor: 10 },
    })
    h.pushBatch(
      batch(1, [
        content("c1", 1, "a"),
        content("c2", 11, "x"),
        content("c1", 2, "b"),
        content("c2", 12, "y"),
      ])
    )
    h.runFrame()
    expect(h.commits).toHaveLength(1)
    expect(h.commits[0].connections).toHaveLength(2)
    expect(h.commits[0].rawEventsInDeliveryOrder.map((e) => e.seq)).toEqual([
      1, 11, 2, 12,
    ])
    expect(h.cursor("c1")).toBe(2)
    expect(h.cursor("c2")).toBe(12)
  })

  it("dispose cancels the pending RAF and makes later pushes no-ops", () => {
    const h = createIngestorHarness({ c1: { key: "tab-1", cursor: 0 } })
    h.pushBatch(batch(1, [content("c1", 1, "a")]))
    expect(h.isScheduled()).toBe(true)
    h.ingestor.dispose()
    expect(h.isScheduled()).toBe(false)
    h.pushBatch(batch(2, [content("c1", 2, "b")]))
    expect(h.isScheduled()).toBe(false)
    h.runFrame()
    expect(h.commits).toHaveLength(0)
  })

  it("thrown commit leaves cursors unadvanced and pauses for recovery", () => {
    const h = createIngestorHarness({ c1: { key: "tab-1", cursor: 0 } })
    h.setCommit(() => {
      throw new Error("commit failed")
    })
    h.pushBatch(batch(1, [content("c1", 1, "a"), content("c1", 2, "b")]))
    expect(() => h.runFrame()).toThrow("commit failed")
    expect(h.cursor("c1")).toBe(0)

    // Still paused — another frame without resume must not commit.
    h.setCommit((frame) => {
      h.commits.push(frame)
      for (const connection of frame.connections) {
        h.setCursor(connection.contextKey, connection.highestSeq)
      }
    })
    h.runFrame()
    expect(h.commits).toHaveLength(0)

    h.ingestor.resumeConnection("c1", 0)
    h.runFrame()
    expect(h.commits).toHaveLength(1)
    expect(h.rawSeqs()).toEqual([1, 2])
    expect(h.cursor("c1")).toBe(2)
  })

  it("pushMapped uses synthetic delivery ids and a provided context key", () => {
    const h = createIngestorHarness({ c1: { key: "tab-1", cursor: 0 } })
    h.ingestor.pushMapped("tab-1", [
      content("c1", 1, "a"),
      content("c1", 2, "b"),
    ])
    h.runFrame()
    expect(h.commits).toHaveLength(1)
    expect(h.commits[0].deliveryIds).toEqual([1])
    expect(h.rawSeqs()).toEqual([1, 2])
  })

  it("flushNow drains immediately without waiting for a frame", () => {
    const h = createIngestorHarness({ c1: { key: "tab-1", cursor: 0 } })
    h.pushBatch(batch(1, [content("c1", 1, "a")]))
    expect(h.commits).toHaveLength(0)
    h.ingestor.flushNow()
    expect(h.commits).toHaveLength(1)
    expect(h.isScheduled()).toBe(false)
  })

  it("does not compact tool_call_update even when not append", () => {
    const h = createIngestorHarness({ c1: { key: "tab-1", cursor: 0 } })
    h.pushBatch(
      batch(1, [
        event("c1", 1, {
          type: "tool_call_update",
          tool_call_id: "tool-1",
          title: "a",
          status: null,
          content: null,
          raw_input: null,
          raw_output: null,
        }),
        event("c1", 2, {
          type: "tool_call_update",
          tool_call_id: "tool-1",
          title: "b",
          status: null,
          content: null,
          raw_input: null,
          raw_output: null,
        }),
      ])
    )
    h.runFrame()
    expect(h.appliedTypes()).toEqual(["tool_call_update", "tool_call_update"])
  })

  it("keeps raw_output_append chunks ordered without merging", () => {
    const h = createIngestorHarness({ c1: { key: "tab-1", cursor: 0 } })
    h.pushBatch(
      batch(1, [
        toolAppend("c1", 1, "chunk-a"),
        toolAppend("c1", 2, "chunk-b"),
        toolAppend("c1", 3, "chunk-c"),
      ])
    )
    h.runFrame()
    expect(h.commits).toHaveLength(1)
    expect(h.appliedTypes()).toEqual([
      "tool_call_update",
      "tool_call_update",
      "tool_call_update",
    ])
    expect(
      h.commits[0].connections[0].applyEvents.map((e) =>
        e.type === "tool_call_update" ? e.raw_output : null
      )
    ).toEqual(["chunk-a", "chunk-b", "chunk-c"])
    expect(h.rawSeqs()).toEqual([1, 2, 3])
  })

  it("snapshot resume mid-queue drops seq <= cursor and applies contiguous suffix", () => {
    const h = createIngestorHarness({ c1: { key: "tab-1", cursor: 5 } })
    h.pushBatch(
      batch(1, [
        content("c1", 6, "a"),
        content("c1", 7, "b"),
        content("c1", 8, "c"),
        content("c1", 9, "d"),
      ])
    )
    expect(h.isScheduled()).toBe(true)
    // Hydrate/resume arrives before the frame commits the queued batch.
    h.setCursor("tab-1", 7)
    h.ingestor.resumeConnection("c1", 7)
    h.runFrame()
    expect(h.commits).toHaveLength(1)
    expect(h.rawSeqs()).toEqual([8, 9])
    expect(h.commits[0].connections[0].applyEvents).toMatchObject([
      { type: "content_delta", text: "cd", seq: 9 },
    ])
    expect(h.cursor("c1")).toBe(9)
  })

  it("gap on A does not starve contiguous B in the same batch", () => {
    const h = createIngestorHarness({
      cA: { key: "tab-a", cursor: 10 },
      cB: { key: "tab-b", cursor: 0 },
    })
    h.pushBatch(
      batch(1, [
        content("cA", 12, "gap"), // missing 11
        content("cB", 1, "x"),
        content("cB", 2, "y"),
        content("cA", 13, "after-gap"),
      ])
    )
    h.runFrame()
    expect(h.gaps).toEqual([
      {
        contextKey: "tab-a",
        connectionId: "cA",
        expectedSeq: 11,
        receivedSeq: 12,
      },
    ])
    // B commits fully; A retains gap events while paused.
    expect(h.commits).toHaveLength(1)
    expect(h.commits[0].connections).toHaveLength(1)
    expect(h.commits[0].connections[0].connectionId).toBe("cB")
    expect(h.rawSeqs()).toEqual([1, 2])
    expect(h.cursor("cB")).toBe(2)
    expect(h.cursor("cA")).toBe(10)

    // Recovery for A must not block B's subsequent contiguous work.
    h.pushBatch(batch(2, [content("cB", 3, "z")]))
    h.runFrame()
    expect(h.cursor("cB")).toBe(3)

    h.setCursor("tab-a", 12)
    h.ingestor.resumeConnection("cA", 12)
    h.runFrame()
    expect(h.rawSeqs().slice(-1)).toEqual([13])
    expect(h.cursor("cA")).toBe(13)
  })

  it("re-resolves context key at drain after reverse-map rekey", () => {
    const h = createIngestorHarness({ c1: { key: "old-key", cursor: 0 } })
    h.pushBatch(batch(1, [content("c1", 1, "a"), content("c1", 2, "b")]))
    // Rekey between receipt and frame commit.
    h.mapConnection("c1", "new-key")
    h.setCursor("new-key", 0)
    h.runFrame()
    expect(h.commits).toHaveLength(1)
    expect(h.commits[0].connections[0].contextKey).toBe("new-key")
    expect(h.cursor("c1")).toBe(2)
    expect(h.rawSeqs()).toEqual([1, 2])
  })
})
