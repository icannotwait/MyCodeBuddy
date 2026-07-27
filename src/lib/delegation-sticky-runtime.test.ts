import { describe, expect, it } from "vitest"
import {
  resolveStickyIdentity,
  stickyIdentityToString,
  foldToolCount,
  applyStickyObservation,
  isLatestStickyCard,
  hasPositiveRecovery,
  STICKY_ORPHAN_TIMEOUT_MS,
  type StickyBucket,
} from "@/lib/delegation-sticky-runtime"

const noRecovery = {
  liveBindingRunning: false,
  childProjectionRunning: false,
  activeRunNonTerminal: false,
  openAttention: false,
  parentWaitingForThisChild: false,
  continueOrReplaceAdmitted: false,
}

function mustBucket(b: StickyBucket | null): StickyBucket {
  expect(b).not.toBeNull()
  return b as StickyBucket
}

describe("identity", () => {
  it("namespaces work_unit with backend+parent", () => {
    const id = resolveStickyIdentity({
      backendCacheKey: "local",
      parentConversationId: 10,
      workUnitKey: "task|1|implementer|grok|none",
      childConversationId: 20,
      taskId: "t1",
    })
    expect(id).toMatchObject({
      backendCacheKey: "local",
      parentConversationId: 10,
      unit: { kind: "work_unit", workUnitKey: "task|1|implementer|grok|none" },
    })
    const s = stickyIdentityToString(id!)
    expect(s).toContain("local")
    expect(s).toContain("10")
  })

  it("refuses bare work_unit without parent", () => {
    expect(
      resolveStickyIdentity({
        backendCacheKey: "local",
        workUnitKey: "task|1|implementer|grok|none",
        taskId: "t1",
      })
    ).toMatchObject({ kind: "task_only", taskId: "t1" })
  })

  it("isolates parallel children keys", () => {
    const a = stickyIdentityToString(
      resolveStickyIdentity({
        backendCacheKey: "local",
        parentConversationId: 1,
        childConversationId: 2,
        taskId: "t1",
      })!
    )
    const b = stickyIdentityToString(
      resolveStickyIdentity({
        backendCacheKey: "local",
        parentConversationId: 1,
        childConversationId: 3,
        taskId: "t1",
      })!
    )
    expect(a).not.toEqual(b)
  })
})

describe("foldToolCount", () => {
  it("A-B-A safe", () => {
    let s = { peakByTaskId: new Map<string, number>() }
    s = foldToolCount(s, "a", 5)
    expect(s.display).toBe(5)
    s = foldToolCount(s, "b", 2)
    expect(s.display).toBe(7)
    s = foldToolCount(s, "a", 5)
    expect(s.display).toBe(7)
  })
})

describe("phase", () => {
  it("parent_turn_failed keeps sticky only with recovery", () => {
    let b = mustBucket(
      applyStickyObservation(null, "k", {
        type: "running",
        taskId: "t1",
        parentToolUseId: "p1",
        startedAt: "2026-07-27T00:00:00.000Z",
        toolCallCount: 1,
        nowMs: 0,
        recovery: { ...noRecovery, liveBindingRunning: true },
      })
    )
    b = mustBucket(
      applyStickyObservation(b, "k", {
        type: "canceled",
        taskId: "t1",
        errorCode: "parent_turn_failed",
        nowMs: 1000,
        recovery: { ...noRecovery, continueOrReplaceAdmitted: true },
      })
    )
    expect(b.phase).toBe("active_sticky")
    b = mustBucket(
      applyStickyObservation(b, "k", {
        type: "canceled",
        taskId: "t1",
        errorCode: "parent_turn_failed",
        nowMs: 2000,
        recovery: noRecovery,
      })
    )
    expect(b.phase).toBe("terminal")
  })

  it("parent_canceled always terminal", () => {
    let b = mustBucket(
      applyStickyObservation(null, "k", {
        type: "running",
        taskId: "t1",
        startedAt: "2026-07-27T00:00:00.000Z",
        toolCallCount: 1,
        nowMs: 0,
        recovery: { ...noRecovery, liveBindingRunning: true },
      })
    )
    b = mustBucket(
      applyStickyObservation(b, "k", {
        type: "canceled",
        taskId: "t1",
        errorCode: "parent_canceled",
        nowMs: 1,
        recovery: { ...noRecovery, liveBindingRunning: true },
      })
    )
    expect(b.phase).toBe("terminal")
  })

  it("cancel_delegation / usercancel terminal", () => {
    let b = mustBucket(
      applyStickyObservation(null, "k", {
        type: "running",
        taskId: "t1",
        startedAt: "2026-07-27T00:00:00.000Z",
        toolCallCount: 1,
        nowMs: 0,
        recovery: { ...noRecovery, liveBindingRunning: true },
      })
    )
    b = mustBucket(
      applyStickyObservation(b, "k", {
        type: "canceled",
        taskId: "t1",
        cancelReason: "usercancel",
        nowMs: 1,
        recovery: noRecovery,
      })
    )
    expect(b.phase).toBe("terminal")
  })

  it("reseed with recovery keeps last display", () => {
    let b = mustBucket(
      applyStickyObservation(null, "k", {
        type: "running",
        taskId: "t1",
        startedAt: "2026-07-27T00:00:00.000Z",
        toolCallCount: 4,
        nowMs: 0,
        recovery: { ...noRecovery, liveBindingRunning: true },
      })
    )
    b = mustBucket(
      applyStickyObservation(b, "k", {
        type: "reseed",
        taskId: "t1",
        nowMs: 500,
        recovery: { ...noRecovery, parentWaitingForThisChild: true },
      })
    )
    expect(b.phase).toBe("active_sticky")
    expect(b.lastDisplayToolCount).toBe(4)
  })

  it("late old terminal does not kill newer active", () => {
    let b = mustBucket(
      applyStickyObservation(null, "k", {
        type: "running",
        taskId: "t1",
        generation: 1,
        parentToolUseId: "p1",
        startedAt: "2026-07-27T00:00:00.000Z",
        toolCallCount: 1,
        nowMs: 0,
        recovery: { ...noRecovery, liveBindingRunning: true },
      })
    )
    b = mustBucket(
      applyStickyObservation(b, "k", {
        type: "running",
        taskId: "t2",
        generation: 2,
        parentToolUseId: "p2",
        startedAt: "2026-07-27T00:02:00.000Z",
        toolCallCount: 1,
        nowMs: 120_000,
        recovery: { ...noRecovery, liveBindingRunning: true },
      })
    )
    b = mustBucket(
      applyStickyObservation(b, "k", {
        type: "completed",
        taskId: "t1",
        generation: 1,
        finishedAt: "2026-07-27T00:03:00.000Z",
        nowMs: 180_000,
        recovery: { ...noRecovery, liveBindingRunning: true },
      })
    )
    expect(b.phase).toBe("active_sticky")
    expect(b.activeTaskId).toBe("t2")
  })

  it("late terminal without generations uses admission order", () => {
    let b = mustBucket(
      applyStickyObservation(null, "k", {
        type: "running",
        taskId: "t1",
        parentToolUseId: "p1",
        startedAt: "2026-07-27T00:00:00.000Z",
        toolCallCount: 1,
        nowMs: 0,
        recovery: { ...noRecovery, liveBindingRunning: true },
      })
    )
    b = mustBucket(
      applyStickyObservation(b, "k", {
        type: "running",
        taskId: "t2",
        parentToolUseId: "p2",
        startedAt: "2026-07-27T00:02:00.000Z",
        toolCallCount: 1,
        nowMs: 120_000,
        recovery: { ...noRecovery, liveBindingRunning: true },
      })
    )
    b = mustBucket(
      applyStickyObservation(b, "k", {
        type: "completed",
        taskId: "t1",
        finishedAt: "2026-07-27T00:03:00.000Z",
        nowMs: 180_000,
        recovery: { ...noRecovery, liveBindingRunning: true },
      })
    )
    expect(b.phase).toBe("active_sticky")
    expect(b.activeTaskId).toBe("t2")
  })

  it("admitting generation-less task clears prior activeGeneration", () => {
    let b = mustBucket(
      applyStickyObservation(null, "k", {
        type: "running",
        taskId: "t1",
        generation: 1,
        parentToolUseId: "p1",
        startedAt: "2026-07-27T00:00:00.000Z",
        toolCallCount: 1,
        nowMs: 0,
        recovery: { ...noRecovery, liveBindingRunning: true },
      })
    )
    b = mustBucket(
      applyStickyObservation(b, "k", {
        type: "running",
        taskId: "t2",
        parentToolUseId: "p2",
        startedAt: "2026-07-27T00:01:00.000Z",
        toolCallCount: 1,
        nowMs: 60_000,
        recovery: { ...noRecovery, liveBindingRunning: true },
      })
    )
    expect(b.activeTaskId).toBe("t2")
    expect(b.activeGeneration).toBeNull()
    expect(
      isLatestStickyCard(
        { taskId: "t1", parentToolUseId: "p1", generation: 1 },
        b
      )
    ).toBe(false)
    expect(
      isLatestStickyCard(
        { taskId: "t2", parentToolUseId: "p2", generation: null },
        b
      )
    ).toBe(true)
  })

  it("completed freezes terminal elapsed from anchor", () => {
    let b = mustBucket(
      applyStickyObservation(null, "k", {
        type: "running",
        taskId: "t1",
        startedAt: "2026-07-27T00:00:00.000Z",
        toolCallCount: 1,
        nowMs: 0,
        recovery: { ...noRecovery, liveBindingRunning: true },
      })
    )
    b = mustBucket(
      applyStickyObservation(b, "k", {
        type: "completed",
        taskId: "t1",
        finishedAt: "2026-07-27T00:01:00.000Z",
        nowMs: 60_000,
        recovery: noRecovery,
      })
    )
    expect(b.phase).toBe("terminal")
    expect(b.terminalElapsedMs).toBe(60_000)
  })

  it("orphan tick terminals after timeout without recovery", () => {
    let b = mustBucket(
      applyStickyObservation(null, "k", {
        type: "running",
        taskId: "t1",
        startedAt: "2026-07-27T00:00:00.000Z",
        toolCallCount: 1,
        nowMs: 0,
        recovery: { ...noRecovery, liveBindingRunning: true },
      })
    )
    b = mustBucket(
      applyStickyObservation(b, "k", {
        type: "canceled",
        taskId: "t1",
        errorCode: "parent_turn_failed",
        nowMs: 1000,
        recovery: { ...noRecovery, continueOrReplaceAdmitted: true },
      })
    )
    // lose recovery → orphan clock starts
    b = mustBucket(
      applyStickyObservation(b, "k", {
        type: "tick",
        taskId: "t1",
        nowMs: 1000,
        recovery: noRecovery,
      })
    )
    expect(b.orphanStartedAtMs).toBe(1000)
    b = mustBucket(
      applyStickyObservation(b, "k", {
        type: "tick",
        taskId: "t1",
        nowMs: 1000 + STICKY_ORPHAN_TIMEOUT_MS,
        recovery: noRecovery,
      })
    )
    expect(b.phase).toBe("terminal")
  })

  it("new child identity does not share bucket state (key differs)", () => {
    const k1 = stickyIdentityToString(
      resolveStickyIdentity({
        backendCacheKey: "local",
        parentConversationId: 1,
        childConversationId: 2,
        taskId: "t1",
      })!
    )
    const k2 = stickyIdentityToString(
      resolveStickyIdentity({
        backendCacheKey: "local",
        parentConversationId: 1,
        childConversationId: 99,
        taskId: "t1",
      })!
    )
    expect(k1).not.toEqual(k2)
  })
})

describe("admitted lineage tool fold", () => {
  it("ignores stats for unadmitted taskIds", () => {
    const b = mustBucket(
      applyStickyObservation(null, "k", {
        type: "running",
        taskId: "t1",
        startedAt: "2026-07-27T00:00:00.000Z",
        toolCallCount: 1,
        nowMs: 0,
        recovery: { ...noRecovery, liveBindingRunning: true },
      })
    )
    const after = mustBucket(
      applyStickyObservation(b, "k", {
        type: "stats",
        taskId: "unknown",
        toolCallCount: 9,
        startedAt: "2026-07-26T00:00:00.000Z",
        nowMs: 1000,
        recovery: { ...noRecovery, liveBindingRunning: true },
      })
    )
    expect(after.lastDisplayToolCount).toBe(1)
    expect(after.peakByTaskId.has("unknown")).toBe(false)
    expect(after.peakByTaskId.get("t1")).toBe(1)
    expect(after.anchorStartedAtMs).toBe(b.anchorStartedAtMs)
    expect(after.taskMeta.has("unknown")).toBe(false)
  })
})

describe("bucket creation boundary", () => {
  const firstObsTypes = [
    "stats",
    "tick",
    "reseed",
    "canceled",
    "completed",
    "failed",
  ] as const

  for (const type of firstObsTypes) {
    it(`first ${type} without prior running does not create bucket`, () => {
      const result = applyStickyObservation(null, "k", {
        type,
        taskId: "t1",
        toolCallCount: 3,
        startedAt: "2026-07-27T00:00:00.000Z",
        finishedAt: "2026-07-27T00:01:00.000Z",
        errorCode: type === "canceled" ? "parent_turn_failed" : undefined,
        nowMs: 0,
        recovery: noRecovery,
      })
      expect(result).toBeNull()
    })
  }

  it("first running creates active_sticky bucket", () => {
    const b = mustBucket(
      applyStickyObservation(null, "k", {
        type: "running",
        taskId: "t1",
        startedAt: "2026-07-27T00:00:00.000Z",
        toolCallCount: 1,
        nowMs: 0,
        recovery: { ...noRecovery, liveBindingRunning: true },
      })
    )
    expect(b.phase).toBe("active_sticky")
    expect(b.activeTaskId).toBe("t1")
  })
})

describe("isLatestStickyCard", () => {
  it("only newer generation is latest", () => {
    let b = mustBucket(
      applyStickyObservation(null, "k", {
        type: "running",
        taskId: "t1",
        generation: 1,
        parentToolUseId: "p1",
        startedAt: "2026-07-27T00:00:00.000Z",
        toolCallCount: 1,
        nowMs: 0,
        recovery: { ...noRecovery, liveBindingRunning: true },
      })
    )
    b = mustBucket(
      applyStickyObservation(b, "k", {
        type: "running",
        taskId: "t2",
        generation: 2,
        parentToolUseId: "p2",
        startedAt: "2026-07-27T00:01:00.000Z",
        toolCallCount: 1,
        nowMs: 60_000,
        recovery: { ...noRecovery, liveBindingRunning: true },
      })
    )
    expect(
      isLatestStickyCard(
        { taskId: "t1", parentToolUseId: "p1", generation: 1 },
        b
      )
    ).toBe(false)
    expect(
      isLatestStickyCard(
        { taskId: "t2", parentToolUseId: "p2", generation: 2 },
        b
      )
    ).toBe(true)
  })
})

describe("hasPositiveRecovery", () => {
  it("any positive signal", () => {
    expect(hasPositiveRecovery(noRecovery)).toBe(false)
    expect(
      hasPositiveRecovery({ ...noRecovery, liveBindingRunning: true })
    ).toBe(true)
  })
})
