import { act, renderHook } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

const ticker = vi.hoisted(() => {
  const release = vi.fn()
  return {
    release,
    retain: vi.fn(() => release),
  }
})

vi.mock("@/hooks/use-delegated-sub-session", () => ({
  useDelegatedSubSession: () => ({
    binding: undefined,
    detail: null,
    loading: false,
    error: null,
  }),
}))

vi.mock("@/contexts/acp-connections-context", () => ({
  useConnectionStore: () => ({
    subscribeKey: () => () => {},
    getConnection: () => undefined,
    getActiveKey: () => null,
    subscribeActiveKey: () => () => {},
  }),
}))

vi.mock("@/lib/delegation-child-projection-cache", async () => {
  const actual = await vi.importActual<
    typeof import("@/lib/delegation-child-projection-cache")
  >("@/lib/delegation-child-projection-cache")
  return {
    ...actual,
    delegationChildProjectionCache: {
      subscribe: () => () => {},
      get: () => null,
      retain: () => () => {},
      ensure: () => {},
    },
  }
})

vi.mock("@/lib/delegation-running-ticker", () => ({
  getRunningTickerVersion: () => 0,
  retainRunningTicker: ticker.retain,
  subscribeRunningTicker: () => () => {},
}))

import { useDelegationCardModel } from "@/hooks/use-delegation-card-model"
import { delegationRunSnapshotCache } from "@/lib/delegation-run-snapshot"
import { getActiveBackendCacheKey } from "@/lib/transport"
import type { DelegationRunSnapshot } from "@/lib/types"

const PARENT_ID = 10
const TASK_ID = "run-3"
const STARTED_AT = "2026-07-19T00:00:00.000Z"

function cacheKey(): string {
  return `${getActiveBackendCacheKey()}\0${PARENT_ID}\0${TASK_ID}`
}

function completedSnapshot(): DelegationRunSnapshot {
  return {
    task_id: TASK_ID,
    root_task_id: "run-1",
    previous_task_id: "run-2",
    generation: 3,
    parent_tool_use_id: "pt-3",
    child_conversation_id: 99,
    agent_type: "codex",
    profile_id: null,
    task_preview: "third review",
    status: "completed",
    error_code: null,
    started_at: STARTED_AT,
    finished_at: "2026-07-19T00:01:00.000Z",
    runtime_stats: null,
    card_summary: null,
    child_turn_anchor: null,
    replaced_task_id: null,
    replacement_reason: null,
  }
}

describe("useDelegationCardModel ticker lifecycle", () => {
  beforeEach(() => {
    ticker.retain.mockClear()
    ticker.release.mockClear()
    delegationRunSnapshotCache.reset()
    vi.spyOn(delegationRunSnapshotCache, "ensure").mockImplementation(() => {})
  })

  it("releases a synthetic running ticker when hydration becomes terminal", () => {
    const { result } = renderHook(() =>
      useDelegationCardModel({
        parentToolUseId: "pt-3",
        parentConversationId: PARENT_ID,
        input: JSON.stringify({ agent_type: "codex", task: "third review" }),
        meta: {
          "codeg.delegation": {
            status: "running",
            task_id: TASK_ID,
            generation: 3,
            started_at: STARTED_AT,
            synthetic_historical: true,
          },
        },
      })
    )

    expect(result.current.lifecycleStatus).toBe("running")
    expect(ticker.retain).toHaveBeenCalledTimes(1)
    expect(ticker.release).not.toHaveBeenCalled()

    act(() => {
      delegationRunSnapshotCache.install(cacheKey(), completedSnapshot())
    })

    expect(result.current.lifecycleStatus).toBe("ok")
    expect(ticker.release).toHaveBeenCalledTimes(1)
  })

  it("retains the ticker while a recent recoverable run awaits continuation", () => {
    const startedAt = new Date(Date.now() - 60_000).toISOString()
    const { result } = renderHook(() =>
      useDelegationCardModel({
        parentToolUseId: "pt-recoverable",
        parentConversationId: PARENT_ID,
        input: JSON.stringify({
          agent_type: "codex",
          task: "continue after parent turn",
          work_unit_key: "unit-recoverable",
        }),
        meta: {
          "codeg.delegation": {
            status: "failed",
            task_id: "run-recoverable",
            child_conversation_id: 99,
            error_code: "parent_turn_failed",
            started_at: startedAt,
            finished_at: startedAt,
            runtime_stats: {
              started_at: startedAt,
              finished_at: startedAt,
              tool_call_count: 3,
              edit_tool_call_count: 0,
              touched_files: [],
              touched_files_truncated: false,
              line_counts_complete: false,
            },
          },
        },
      })
    )

    expect(result.current.lifecycleStatus).toBe("running")
    expect(result.current.errorCode).toBeUndefined()
    expect(result.current.showGeneratingSegment).toBe(true)
    expect(ticker.retain).toHaveBeenCalledTimes(1)
  })
})
