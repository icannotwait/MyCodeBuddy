import { describe, expect, it, vi, beforeEach, afterEach } from "vitest"
import {
  observeSticky,
  getStickySnapshot,
  subscribeSticky,
  resetStickyBackend,
} from "@/lib/delegation-sticky-store"
import { STICKY_ORPHAN_TIMEOUT_MS } from "@/lib/delegation-sticky-runtime"
import { resetBackendScopedStores } from "@/stores/backend-scoped-store-reset"

const recoveryOn = {
  liveBindingRunning: true,
  childProjectionRunning: false,
  activeRunNonTerminal: true,
  openAttention: false,
  parentWaitingForThisChild: false,
  continueOrReplaceAdmitted: false,
}

const recoveryOff = {
  liveBindingRunning: false,
  childProjectionRunning: false,
  activeRunNonTerminal: false,
  openAttention: false,
  parentWaitingForThisChild: false,
  continueOrReplaceAdmitted: false,
}

beforeEach(() => {
  // Full clear via the module’s backend-scoped registration.
  resetBackendScopedStores()
})

afterEach(() => {
  vi.useRealTimers()
  resetBackendScopedStores()
})

describe("sticky store", () => {
  it("returns identityKey and notifies subscribers", () => {
    const spy = vi.fn()
    const unsub = subscribeSticky(spy)
    const result = observeSticky({
      backendCacheKey: "local",
      parentConversationId: 1,
      childConversationId: 2,
      type: "running",
      taskId: "t1",
      parentToolUseId: "p1",
      startedAt: "2026-07-27T00:00:00.000Z",
      toolCallCount: 1,
      nowMs: 0,
      recovery: recoveryOn,
    })
    expect(result?.identityKey).toBeTruthy()
    expect(getStickySnapshot(result!.identityKey)?.phase).toBe("active_sticky")
    expect(spy).toHaveBeenCalled()
    unsub()
  })

  it("resetStickyBackend clears only that backend", () => {
    const a = observeSticky({
      backendCacheKey: "backend-a",
      parentConversationId: 1,
      childConversationId: 2,
      type: "running",
      taskId: "t1",
      startedAt: "2026-07-27T00:00:00.000Z",
      toolCallCount: 1,
      nowMs: 0,
      recovery: recoveryOn,
    })!
    const b = observeSticky({
      backendCacheKey: "backend-b",
      parentConversationId: 1,
      childConversationId: 2,
      type: "running",
      taskId: "t1",
      startedAt: "2026-07-27T00:00:00.000Z",
      toolCallCount: 1,
      nowMs: 0,
      recovery: recoveryOn,
    })!
    resetStickyBackend("backend-a")
    expect(getStickySnapshot(a.identityKey)).toBeUndefined()
    expect(getStickySnapshot(b.identityKey)?.phase).toBe("active_sticky")
  })

  it("registers with backend-scoped store reset", () => {
    const seeded = observeSticky({
      backendCacheKey: "local",
      parentConversationId: 9,
      childConversationId: 9,
      type: "running",
      taskId: "t9",
      startedAt: "2026-07-27T00:00:00.000Z",
      toolCallCount: 1,
      nowMs: 0,
      recovery: recoveryOn,
    })!
    resetBackendScopedStores()
    expect(getStickySnapshot(seeded.identityKey)).toBeUndefined()
  })

  it("orphan timer fires with fake timers even without valid startedAt later", () => {
    vi.useFakeTimers()

    // Seed active_sticky with recovery.
    const seeded = observeSticky({
      backendCacheKey: "local",
      parentConversationId: 1,
      childConversationId: 2,
      type: "running",
      taskId: "t1",
      parentToolUseId: "p1",
      startedAt: "2026-07-27T00:00:00.000Z",
      toolCallCount: 1,
      nowMs: 0,
      recovery: recoveryOn,
    })!
    expect(getStickySnapshot(seeded.identityKey)?.phase).toBe("active_sticky")

    // Recovery-gated cancel still sticky while recovery-owned.
    observeSticky({
      backendCacheKey: "local",
      parentConversationId: 1,
      childConversationId: 2,
      type: "canceled",
      taskId: "t1",
      errorCode: "parent_turn_failed",
      nowMs: 1000,
      recovery: recoveryOn,
    })
    expect(getStickySnapshot(seeded.identityKey)?.phase).toBe("active_sticky")
    expect(getStickySnapshot(seeded.identityKey)?.orphanStartedAtMs).toBeNull()

    // Lose all recovery without valid startedAt → orphan clock starts, stays active.
    observeSticky({
      backendCacheKey: "local",
      parentConversationId: 1,
      childConversationId: 2,
      type: "tick",
      taskId: "t1",
      nowMs: 2000,
      recovery: recoveryOff,
    })
    expect(getStickySnapshot(seeded.identityKey)?.phase).toBe("active_sticky")
    expect(getStickySnapshot(seeded.identityKey)?.orphanStartedAtMs).toBe(2000)

    // Store timer must fire even though no further startedAt / ticker is eligible.
    vi.advanceTimersByTime(STICKY_ORPHAN_TIMEOUT_MS)
    expect(getStickySnapshot(seeded.identityKey)?.phase).toBe("terminal")
    expect(getStickySnapshot(seeded.identityKey)?.orphanStartedAtMs).toBeNull()
  })

  it("getSnapshot referential stability when unchanged", () => {
    const r = observeSticky({
      backendCacheKey: "local",
      parentConversationId: 3,
      childConversationId: 4,
      type: "running",
      taskId: "t1",
      startedAt: "2026-07-27T00:00:00.000Z",
      toolCallCount: 1,
      nowMs: 0,
      recovery: recoveryOn,
    })!
    const s1 = getStickySnapshot(r.identityKey)
    const s2 = getStickySnapshot(r.identityKey)
    expect(s1).toBe(s2)
  })

  it("notifies two subscribers independently", () => {
    const a = vi.fn()
    const b = vi.fn()
    const unsubA = subscribeSticky(a)
    const unsubB = subscribeSticky(b)

    observeSticky({
      backendCacheKey: "local",
      parentConversationId: 1,
      childConversationId: 2,
      type: "running",
      taskId: "t1",
      startedAt: "2026-07-27T00:00:00.000Z",
      toolCallCount: 1,
      nowMs: 0,
      recovery: recoveryOn,
    })

    expect(a).toHaveBeenCalledTimes(1)
    expect(b).toHaveBeenCalledTimes(1)
    unsubA()
    unsubB()
  })

  it("unsubscribe stops notifications; remount receives again", () => {
    const spy = vi.fn()
    const unsub = subscribeSticky(spy)

    observeSticky({
      backendCacheKey: "local",
      parentConversationId: 1,
      childConversationId: 2,
      type: "running",
      taskId: "t1",
      startedAt: "2026-07-27T00:00:00.000Z",
      toolCallCount: 1,
      nowMs: 0,
      recovery: recoveryOn,
    })
    expect(spy).toHaveBeenCalledTimes(1)

    unsub()
    spy.mockClear()

    observeSticky({
      backendCacheKey: "local",
      parentConversationId: 1,
      childConversationId: 2,
      type: "stats",
      taskId: "t1",
      toolCallCount: 2,
      nowMs: 10,
      recovery: recoveryOn,
    })
    expect(spy).not.toHaveBeenCalled()

    const remount = subscribeSticky(spy)
    observeSticky({
      backendCacheKey: "local",
      parentConversationId: 1,
      childConversationId: 2,
      type: "stats",
      taskId: "t1",
      toolCallCount: 3,
      nowMs: 20,
      recovery: recoveryOn,
    })
    expect(spy).toHaveBeenCalledTimes(1)
    remount()
  })

  it("StrictMode double-observe is idempotent for the same running frame", () => {
    // React StrictMode re-runs effects: two observeSticky calls with the same
    // running observation must not double tool peaks or corrupt phase.
    const input = {
      backendCacheKey: "local",
      parentConversationId: 1,
      childConversationId: 2,
      type: "running" as const,
      taskId: "t1",
      parentToolUseId: "p1",
      startedAt: "2026-07-27T00:00:00.000Z",
      toolCallCount: 4,
      nowMs: 0,
      recovery: recoveryOn,
    }

    const first = observeSticky(input)!
    const afterFirst = getStickySnapshot(first.identityKey)!
    const second = observeSticky(input)!
    const afterSecond = getStickySnapshot(second.identityKey)!

    expect(second.identityKey).toBe(first.identityKey)
    expect(afterSecond.phase).toBe("active_sticky")
    expect(afterSecond.lastDisplayToolCount).toBe(4)
    expect(afterSecond.peakByTaskId.get("t1")).toBe(4)
    expect(afterSecond.activeTaskId).toBe(afterFirst.activeTaskId)
    expect(afterSecond.anchorStartedAtMs).toBe(afterFirst.anchorStartedAtMs)
  })
})
