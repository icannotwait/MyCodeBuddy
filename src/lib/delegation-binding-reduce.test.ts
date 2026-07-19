import { describe, expect, it } from "vitest"

import {
  applyDelegationEnvelope,
  type DelegationBinding,
} from "@/lib/delegation-binding-reduce"
import {
  emptyRuntimeStats,
  type AttentionRequestSummary,
  type EventEnvelope,
} from "@/lib/types"

const STARTED_AT = "2026-07-19T00:00:00.000Z"
const FINISHED_AT = "2026-07-19T00:01:30.000Z"

const ATTENTION: AttentionRequestSummary = {
  request_id: "req-1",
  task_id: "task-1",
  message: "Need parent decision",
  created_at: STARTED_AT,
}

function runningBinding(
  overrides: Partial<DelegationBinding> = {}
): DelegationBinding {
  return {
    parentConnectionId: "p1",
    parentToolUseId: "pt-1",
    childConnectionId: "c1",
    childConversationId: 99,
    agentType: "codex",
    status: "running",
    taskId: "task-1",
    startedAt: STARTED_AT,
    runtimeStats: emptyRuntimeStats(STARTED_AT),
    attentionRequest: null,
    observation: "active",
    lastAgentActivityAt: null,
    stalledSince: null,
    ...overrides,
  }
}

function mapOf(...bindings: DelegationBinding[]): Map<string, DelegationBinding> {
  const m = new Map<string, DelegationBinding>()
  for (const b of bindings) m.set(b.parentToolUseId, b)
  return m
}

function started(
  overrides: Partial<{
    parent_tool_use_id: string
    task_id: string
    started_at: string
    observation: "active" | "waiting_input" | "stalled"
  }> = {}
): EventEnvelope {
  return {
    seq: 1,
    connection_id: "p1",
    type: "delegation_started",
    parent_connection_id: "p1",
    parent_tool_use_id: "pt-1",
    child_connection_id: "c1",
    child_conversation_id: 99,
    agent_type: "codex",
    task_id: "task-1",
    started_at: STARTED_AT,
    runtime_stats: emptyRuntimeStats(STARTED_AT),
    ...overrides,
  }
}

function completed(
  result:
    | { kind: "ok"; duration_ms: number; text_preview?: string | null }
    | { kind: "err"; error_code: string },
  overrides: Partial<{
    parent_tool_use_id: string
    task_id: string
    runtime_stats: ReturnType<typeof emptyRuntimeStats>
  }> = {}
): EventEnvelope {
  return {
    seq: 2,
    connection_id: "p1",
    type: "delegation_completed",
    parent_connection_id: "p1",
    parent_tool_use_id: "pt-1",
    child_connection_id: "c1",
    child_conversation_id: 99,
    agent_type: "codex",
    task_id: "task-1",
    runtime_stats: {
      ...emptyRuntimeStats(STARTED_AT),
      finished_at: FINISHED_AT,
      tool_call_count: 3,
    },
    result,
    ...overrides,
  }
}

describe("applyDelegationEnvelope", () => {
  it("installs started binding with taskId, startedAt, runtimeStats, attention", () => {
    const stats = {
      ...emptyRuntimeStats(STARTED_AT),
      tool_call_count: 1,
    }
    const envelope: EventEnvelope = {
      seq: 1,
      connection_id: "p1",
      type: "delegation_started",
      parent_connection_id: "p1",
      parent_tool_use_id: "pt-1",
      child_connection_id: "c1",
      child_conversation_id: 99,
      agent_type: "codex",
      task_id: "task-1",
      started_at: STARTED_AT,
      runtime_stats: stats,
      attention_request: ATTENTION,
    }
    const { next, acceptedCompletionToolUseId } = applyDelegationEnvelope(
      new Map(),
      envelope
    )
    expect(acceptedCompletionToolUseId).toBeNull()
    const b = next.get("pt-1")
    expect(b).toMatchObject({
      status: "running",
      taskId: "task-1",
      startedAt: STARTED_AT,
      runtimeStats: stats,
      attentionRequest: ATTENTION,
      observation: "active",
    })
  })

  it("replaces runtimeStats on matching task_id", () => {
    const prev = mapOf(runningBinding())
    const stats = {
      ...emptyRuntimeStats(STARTED_AT),
      tool_call_count: 7,
      edit_tool_call_count: 2,
    }
    const { next, acceptedCompletionToolUseId } = applyDelegationEnvelope(
      prev,
      {
        seq: 3,
        connection_id: "p1",
        type: "delegation_runtime_stats_changed",
        parent_tool_use_id: "pt-1",
        task_id: "task-1",
        runtime_stats: stats,
      }
    )
    expect(acceptedCompletionToolUseId).toBeNull()
    expect(next.get("pt-1")?.runtimeStats).toEqual(stats)
    expect(next).not.toBe(prev)
  })

  it("ignores runtime_stats_changed when task_id mismatches", () => {
    const prev = mapOf(runningBinding())
    const { next, acceptedCompletionToolUseId } = applyDelegationEnvelope(
      prev,
      {
        seq: 3,
        connection_id: "p1",
        type: "delegation_runtime_stats_changed",
        parent_tool_use_id: "pt-1",
        task_id: "stale-task",
        runtime_stats: {
          ...emptyRuntimeStats(STARTED_AT),
          tool_call_count: 99,
        },
      }
    )
    expect(acceptedCompletionToolUseId).toBeNull()
    expect(next).toBe(prev)
    expect(next.get("pt-1")?.runtimeStats.tool_call_count).toBe(0)
  })

  it("ignores runtime_stats_changed when binding is absent", () => {
    const prev = new Map<string, DelegationBinding>()
    const { next } = applyDelegationEnvelope(prev, {
      seq: 3,
      connection_id: "p1",
      type: "delegation_runtime_stats_changed",
      parent_tool_use_id: "pt-1",
      task_id: "task-1",
      runtime_stats: emptyRuntimeStats(STARTED_AT),
    })
    expect(next).toBe(prev)
  })

  it("sets attention on match and clears when attention_request is null", () => {
    const prev = mapOf(runningBinding({ attentionRequest: ATTENTION }))
    const cleared = applyDelegationEnvelope(prev, {
      seq: 4,
      connection_id: "p1",
      type: "delegation_attention_changed",
      parent_tool_use_id: "pt-1",
      task_id: "task-1",
      attention_request: null,
    })
    expect(cleared.next.get("pt-1")?.attentionRequest).toBeNull()
    expect(cleared.acceptedCompletionToolUseId).toBeNull()
  })

  it("clears attention when attention_request is omitted", () => {
    const prev = mapOf(runningBinding({ attentionRequest: ATTENTION }))
    const { next } = applyDelegationEnvelope(prev, {
      seq: 4,
      connection_id: "p1",
      type: "delegation_attention_changed",
      parent_tool_use_id: "pt-1",
      task_id: "task-1",
      // attention_request intentionally omitted
    })
    expect(next.get("pt-1")?.attentionRequest).toBeNull()
  })

  it("ignores attention_changed when task_id mismatches", () => {
    const prev = mapOf(runningBinding({ attentionRequest: ATTENTION }))
    const { next } = applyDelegationEnvelope(prev, {
      seq: 4,
      connection_id: "p1",
      type: "delegation_attention_changed",
      parent_tool_use_id: "pt-1",
      task_id: "other",
      attention_request: null,
    })
    expect(next).toBe(prev)
    expect(next.get("pt-1")?.attentionRequest).toEqual(ATTENTION)
  })

  it("applies observation only when running and task_id matches", () => {
    const prev = mapOf(runningBinding())
    const { next } = applyDelegationEnvelope(prev, {
      seq: 5,
      connection_id: "p1",
      type: "delegation_observation_changed",
      parent_tool_use_id: "pt-1",
      task_id: "task-1",
      observation: "stalled",
      last_agent_activity_at: "2026-07-19T00:00:30.000Z",
      stalled_since: "2026-07-19T00:00:40.000Z",
    })
    expect(next.get("pt-1")).toMatchObject({
      status: "running",
      observation: "stalled",
      lastAgentActivityAt: "2026-07-19T00:00:30.000Z",
      stalledSince: "2026-07-19T00:00:40.000Z",
    })
  })

  it("ignores observation when task_id mismatches", () => {
    const prev = mapOf(runningBinding({ observation: "active" }))
    const { next } = applyDelegationEnvelope(prev, {
      seq: 5,
      connection_id: "p1",
      type: "delegation_observation_changed",
      parent_tool_use_id: "pt-1",
      task_id: "stale",
      observation: "stalled",
      last_agent_activity_at: "2026-07-19T00:00:30.000Z",
    })
    expect(next).toBe(prev)
    expect(next.get("pt-1")?.observation).toBe("active")
  })

  it("ignores observation on terminal binding", () => {
    const prev = mapOf(
      runningBinding({
        status: "ok",
        observation: null,
      })
    )
    const { next } = applyDelegationEnvelope(prev, {
      seq: 5,
      connection_id: "p1",
      type: "delegation_observation_changed",
      parent_tool_use_id: "pt-1",
      task_id: "task-1",
      observation: "stalled",
      last_agent_activity_at: "2026-07-19T00:00:30.000Z",
    })
    expect(next).toBe(prev)
  })

  it("accepts matching completion ok with stats, duration, finishedAt; clears attention", () => {
    const prev = mapOf(
      runningBinding({
        attentionRequest: ATTENTION,
        observation: "waiting_input",
      })
    )
    const stats = {
      ...emptyRuntimeStats(STARTED_AT),
      finished_at: FINISHED_AT,
      tool_call_count: 4,
    }
    const { next, acceptedCompletionToolUseId } = applyDelegationEnvelope(
      prev,
      completed(
        { kind: "ok", duration_ms: 1500 },
        { runtime_stats: stats }
      )
    )
    expect(acceptedCompletionToolUseId).toBe("pt-1")
    expect(next.get("pt-1")).toMatchObject({
      status: "ok",
      runtimeStats: stats,
      finishedAt: FINISHED_AT,
      completedDurationMs: 1500,
      attentionRequest: null,
      observation: null,
      stalledSince: null,
    })
  })

  it("accepts matching completion err; completedDurationMs is null", () => {
    const prev = mapOf(runningBinding())
    const { next, acceptedCompletionToolUseId } = applyDelegationEnvelope(
      prev,
      completed({ kind: "err", error_code: "canceled" })
    )
    expect(acceptedCompletionToolUseId).toBe("pt-1")
    expect(next.get("pt-1")).toMatchObject({
      status: "err",
      errorCode: "canceled",
      completedDurationMs: null,
      finishedAt: FINISHED_AT,
    })
  })

  it("rejects mismatched completion: no map change, no accepted id", () => {
    const prev = mapOf(runningBinding({ taskId: "task-1" }))
    const { next, acceptedCompletionToolUseId } = applyDelegationEnvelope(
      prev,
      completed(
        { kind: "ok", duration_ms: 100 },
        { task_id: "other-task" }
      )
    )
    expect(acceptedCompletionToolUseId).toBeNull()
    expect(next).toBe(prev)
    expect(next.get("pt-1")?.status).toBe("running")
  })

  it("synthesizes terminal binding when completion arrives with no prior start", () => {
    const stats = {
      ...emptyRuntimeStats(STARTED_AT),
      finished_at: FINISHED_AT,
      tool_call_count: 2,
    }
    const { next, acceptedCompletionToolUseId } = applyDelegationEnvelope(
      new Map(),
      completed(
        { kind: "err", error_code: "timeout" },
        { runtime_stats: stats }
      )
    )
    expect(acceptedCompletionToolUseId).toBe("pt-1")
    expect(next.get("pt-1")).toMatchObject({
      status: "err",
      errorCode: "timeout",
      taskId: "task-1",
      agentType: "codex",
      childConversationId: 99,
      startedAt: STARTED_AT,
      runtimeStats: stats,
      finishedAt: FINISHED_AT,
      completedDurationMs: null,
      attentionRequest: null,
    })
  })

  it("started always replaces binding for parent_tool_use_id (new task wins)", () => {
    const prev = mapOf(
      runningBinding({
        taskId: "old-task",
        status: "ok",
        finishedAt: FINISHED_AT,
        completedDurationMs: 50,
      })
    )
    const { next, acceptedCompletionToolUseId } = applyDelegationEnvelope(
      prev,
      started({ task_id: "new-task" })
    )
    expect(acceptedCompletionToolUseId).toBeNull()
    expect(next.get("pt-1")).toMatchObject({
      status: "running",
      taskId: "new-task",
      finishedAt: null,
      completedDurationMs: null,
    })
  })
})
