import { describe, expect, it } from "vitest"

import {
  buildDelegationWorkUnitRuntime,
  STICKY_ORPHAN_TIMEOUT_MS,
  type WorkUnitRunObservation,
} from "@/lib/delegation-work-unit-runtime"
import type { DelegationRuntimeStats } from "@/lib/types"

const START = "2026-07-27T00:00:00.000Z"

function runtimeStats(
  overrides: Partial<DelegationRuntimeStats> = {}
): DelegationRuntimeStats {
  return {
    started_at: START,
    tool_call_count: 0,
    edit_tool_call_count: 0,
    touched_files: [],
    touched_files_truncated: false,
    line_counts_complete: false,
    ...overrides,
  }
}

function observed(
  identity: string,
  overrides: Partial<WorkUnitRunObservation> = {}
): WorkUnitRunObservation {
  return {
    identity,
    taskId: identity,
    lifecycleStatus: "err",
    errorCode: "parent_turn_failed",
    startedAt: START,
    finishedAt: "2026-07-27T00:05:00.000Z",
    lastAgentActivityAt: null,
    runtimeStats: null,
    current: false,
    ...overrides,
  }
}

describe("buildDelegationWorkUnitRuntime", () => {
  it("deduplicates observations by run and sums per-run metric peaks", () => {
    const result = buildDelegationWorkUnitRuntime({
      runs: [
        observed("run-1:first", {
          taskId: "run-1",
          runtimeStats: runtimeStats({
            tool_call_count: 5,
            edit_tool_call_count: 1,
            additions: 10,
            deletions: 2,
            line_counts_complete: true,
            touched_files: [
              {
                path: "src/a.ts",
                outside_workspace: false,
                additions: 1,
                deletions: 0,
              },
            ],
          }),
        }),
        observed("run-1:later", {
          taskId: "run-1",
          runtimeStats: runtimeStats({
            tool_call_count: 3,
            edit_tool_call_count: 4,
            additions: 8,
            deletions: 6,
            line_counts_complete: true,
            touched_files: [
              {
                path: "src/a.ts",
                outside_workspace: false,
                additions: 5,
                deletions: 1,
              },
              { path: "src/b.ts", outside_workspace: false },
            ],
          }),
        }),
        observed("run-2", {
          lifecycleStatus: "running",
          errorCode: null,
          startedAt: "2026-07-27T00:05:00.000Z",
          finishedAt: null,
          runtimeStats: runtimeStats({
            started_at: "2026-07-27T00:05:00.000Z",
            tool_call_count: 2,
            edit_tool_call_count: 2,
            additions: 4,
            deletions: 1,
            line_counts_complete: true,
            touched_files_truncated: true,
            touched_files: [{ path: "src/c.ts", outside_workspace: false }],
          }),
          current: true,
        }),
      ],
      nowMs: Date.parse("2026-07-27T00:06:00.000Z"),
      hasLiveBinding: true,
      explicitUserCancel: false,
    })

    // Continuous runs: run-1 [0,5m] + open run-2 [5m,6m] = 6m active sum.
    expect(result).toMatchObject({
      activeSticky: true,
      startedAt: START,
      finishedAt: null,
      elapsedMs: 360_000,
      toolCallCount: 7,
      lifecycleOverride: "running",
      statusOverride: "running",
      suppressErrorCode: false,
    })
    expect(result.runtimeStats).toMatchObject({
      started_at: START,
      finished_at: null,
      tool_call_count: 7,
      edit_tool_call_count: 6,
      additions: 14,
      deletions: 7,
      line_counts_complete: true,
      touched_files_truncated: true,
    })
    expect(result.runtimeStats?.touched_files).toEqual([
      {
        path: "src/a.ts",
        outside_workspace: false,
        additions: 5,
        deletions: 1,
      },
      { path: "src/b.ts", outside_workspace: false },
      { path: "src/c.ts", outside_workspace: false },
    ])
  })

  it("keeps a recoverable transition sticky until the orphan deadline", () => {
    const finishedAt = "2026-07-27T00:05:00.000Z"
    const beforeDeadline = Date.parse(finishedAt) + STICKY_ORPHAN_TIMEOUT_MS - 1
    const result = buildDelegationWorkUnitRuntime({
      runs: [observed("run-1", { current: true, finishedAt })],
      nowMs: beforeDeadline,
      hasLiveBinding: false,
      explicitUserCancel: false,
    })

    expect(result.activeSticky).toBe(true)
    expect(result.lifecycleOverride).toBe("running")
    expect(result.statusOverride).toBe("running")
    expect(result.suppressErrorCode).toBe(true)
    // Idle orphan wait is not billed — elapsed freezes at the run's own span.
    expect(result.elapsedMs).toBe(300_000)
  })

  it("uses the latest completeness flag for repeated snapshots of one run", () => {
    const result = buildDelegationWorkUnitRuntime({
      runs: [
        observed("run-1:first", {
          taskId: "run-1",
          runtimeStats: runtimeStats({
            additions: 1,
            deletions: 1,
            line_counts_complete: true,
          }),
        }),
        observed("run-1:latest", {
          taskId: "run-1",
          runtimeStats: runtimeStats({
            additions: 2,
            deletions: 2,
            line_counts_complete: false,
          }),
          current: true,
        }),
      ],
      nowMs: Date.parse("2026-07-27T00:06:00.000Z"),
      hasLiveBinding: false,
      explicitUserCancel: false,
    })

    expect(result.runtimeStats?.line_counts_complete).toBe(false)
  })

  it("releases a recoverable orphan exactly at 900000ms without live evidence", () => {
    const finishedAt = "2026-07-27T00:05:00.000Z"
    const result = buildDelegationWorkUnitRuntime({
      runs: [observed("run-1", { current: true, finishedAt })],
      nowMs: Date.parse(finishedAt) + STICKY_ORPHAN_TIMEOUT_MS,
      hasLiveBinding: false,
      explicitUserCancel: false,
    })

    expect(result.activeSticky).toBe(false)
    expect(result.lifecycleOverride).toBeNull()
    expect(result.statusOverride).toBeNull()
    expect(result.suppressErrorCode).toBe(false)
    expect(result.elapsedMs).toBe(300_000)
  })

  it("keeps a recoverable transition sticky while a live binding exists", () => {
    const result = buildDelegationWorkUnitRuntime({
      runs: [observed("run-1", { current: true })],
      nowMs:
        Date.parse("2026-07-27T00:05:00.000Z") + STICKY_ORPHAN_TIMEOUT_MS * 2,
      hasLiveBinding: true,
      explicitUserCancel: false,
    })

    expect(result.activeSticky).toBe(true)
  })

  it("does not start a historical sticky window without persisted time evidence", () => {
    const result = buildDelegationWorkUnitRuntime({
      runs: [
        observed("run-1", {
          startedAt: null,
          finishedAt: null,
          lastAgentActivityAt: null,
          current: true,
        }),
      ],
      nowMs: Date.parse("2026-07-27T00:06:00.000Z"),
      hasLiveBinding: false,
      explicitUserCancel: false,
    })

    expect(result.activeSticky).toBe(false)
    expect(result.startedAt).toBeNull()
    expect(result.elapsedMs).toBeNull()
  })

  it.each([
    ["completed", "ok", null, false],
    ["business failure", "err", "tests_failed", false],
    ["user cancellation", "err", "user_cancelled", false],
    ["explicit parent cancellation", "err", "parent_canceled", true],
  ] as const)(
    "keeps %s terminal",
    (_label, lifecycleStatus, errorCode, explicitUserCancel) => {
      const result = buildDelegationWorkUnitRuntime({
        runs: [
          observed("run-1", {
            lifecycleStatus,
            errorCode,
            current: true,
          }),
        ],
        nowMs: Date.parse("2026-07-27T00:06:00.000Z"),
        hasLiveBinding: true,
        explicitUserCancel,
      })

      expect(result.activeSticky).toBe(false)
      expect(result.lifecycleOverride).toBeNull()
      expect(result.statusOverride).toBeNull()
    }
  )

  it("treats parent_canceled as recoverable only without an explicit stop", () => {
    const result = buildDelegationWorkUnitRuntime({
      runs: [
        observed("run-1", {
          errorCode: "parent_canceled",
          current: true,
        }),
      ],
      nowMs: Date.parse("2026-07-27T00:06:00.000Z"),
      hasLiveBinding: false,
      explicitUserCancel: false,
    })

    expect(result.activeSticky).toBe(true)
    expect(result.suppressErrorCode).toBe(true)
  })

  it.each(["parent_turn_failed", "join_abandoned", "parent_disconnected"])(
    "keeps the %s orchestration transition recoverable",
    (errorCode) => {
      const result = buildDelegationWorkUnitRuntime({
        runs: [observed("run-1", { errorCode, current: true })],
        nowMs: Date.parse("2026-07-27T00:06:00.000Z"),
        hasLiveBinding: false,
        explicitUserCancel: false,
      })

      expect(result.activeSticky).toBe(true)
      expect(result.suppressErrorCode).toBe(true)
    }
  )

  it("sums per-run active spans and excludes idle gaps between turns", () => {
    // run-1: 0–2m, idle 2–10m, run-2: 10–12m → total active 4m, not wall 12m.
    const result = buildDelegationWorkUnitRuntime({
      runs: [
        observed("run-1", {
          lifecycleStatus: "ok",
          errorCode: null,
          startedAt: START,
          finishedAt: "2026-07-27T00:02:00.000Z",
          runtimeStats: runtimeStats({
            finished_at: "2026-07-27T00:02:00.000Z",
            tool_call_count: 1,
          }),
        }),
        observed("run-2", {
          lifecycleStatus: "ok",
          errorCode: null,
          startedAt: "2026-07-27T00:10:00.000Z",
          finishedAt: "2026-07-27T00:12:00.000Z",
          runtimeStats: runtimeStats({
            started_at: "2026-07-27T00:10:00.000Z",
            finished_at: "2026-07-27T00:12:00.000Z",
            tool_call_count: 2,
          }),
          current: true,
        }),
      ],
      nowMs: Date.parse("2026-07-27T00:12:00.000Z"),
      hasLiveBinding: false,
      explicitUserCancel: false,
    })

    expect(result.activeSticky).toBe(false)
    expect(result.startedAt).toBe(START)
    expect(result.finishedAt).toBe("2026-07-27T00:12:00.000Z")
    expect(result.elapsedMs).toBe(240_000)
    expect(result.toolCallCount).toBe(3)
  })

  it("never invents a zero tool count when runtime stats are absent", () => {
    const result = buildDelegationWorkUnitRuntime({
      runs: [
        observed("run-1", {
          lifecycleStatus: "running",
          errorCode: null,
          finishedAt: null,
          current: true,
        }),
      ],
      nowMs: Date.parse("2026-07-27T00:01:00.000Z"),
      hasLiveBinding: true,
      explicitUserCancel: false,
    })

    expect(result.toolCallCount).toBeNull()
    expect(result.runtimeStats).toBeNull()
    expect(result.activeSticky).toBe(true)
  })

  it("omits runtime output when every observed start time is invalid", () => {
    const result = buildDelegationWorkUnitRuntime({
      runs: [
        observed("run-1", {
          lifecycleStatus: "running",
          errorCode: null,
          startedAt: "not-a-time",
          finishedAt: null,
          runtimeStats: runtimeStats({
            started_at: "also-not-a-time",
            tool_call_count: 3,
          }),
          current: true,
        }),
      ],
      nowMs: Date.parse("2026-07-27T00:01:00.000Z"),
      hasLiveBinding: true,
      explicitUserCancel: false,
    })

    expect(result.startedAt).toBeNull()
    expect(result.elapsedMs).toBeNull()
    expect(result.toolCallCount).toBe(3)
    expect(result.runtimeStats).toBeNull()
  })

  it("keeps stable file order while the latest observation replaces details", () => {
    const result = buildDelegationWorkUnitRuntime({
      runs: [
        observed("first", {
          runtimeStats: runtimeStats({
            tool_call_count: 1,
            touched_files: [
              { path: "same.ts", outside_workspace: false, additions: 1 },
              { path: "first.ts", outside_workspace: false },
            ],
          }),
        }),
        observed("second", {
          runtimeStats: runtimeStats({
            tool_call_count: 1,
            touched_files: [
              {
                path: "same.ts",
                outside_workspace: true,
                additions: 9,
                deletions: 2,
              },
              { path: "second.ts", outside_workspace: false },
            ],
          }),
          current: true,
        }),
      ],
      nowMs: Date.parse("2026-07-27T00:06:00.000Z"),
      hasLiveBinding: false,
      explicitUserCancel: false,
    })

    expect(result.runtimeStats?.touched_files).toEqual([
      {
        path: "same.ts",
        outside_workspace: true,
        additions: 9,
        deletions: 2,
      },
      { path: "first.ts", outside_workspace: false },
      { path: "second.ts", outside_workspace: false },
    ])
  })
})
