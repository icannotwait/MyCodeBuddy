import { describe, expect, it } from "vitest"

import {
  buildDelegationCardModel,
  isTickerEligible,
  mergeDelegationWorkUnitModel,
  scopeDelegationBindingForCard,
  type DelegationCardSource,
} from "@/hooks/use-delegation-card-model"
import type { DelegationBinding } from "@/lib/delegation-binding-reduce"
import type { ChildCardProjection } from "@/lib/delegation-child-projection-cache"
import {
  parseInput,
  type ParsedMeta,
  type ParsedToolOutput,
} from "@/lib/delegation-card"
import {
  emptyRuntimeStats,
  type AttentionRequestSummary,
  type CardSummary,
  type DelegationRunSnapshot,
  type DelegationRuntimeStats,
} from "@/lib/types"

const STARTED_AT = "2026-07-19T00:00:00.000Z"
const FINISHED_AT = "2026-07-19T00:01:30.000Z"
const NOW_MS = Date.parse("2026-07-19T00:02:00.000Z")

const ATTENTION: AttentionRequestSummary = {
  request_id: "req-1",
  task_id: "task-1",
  message: "Need parent decision",
  created_at: STARTED_AT,
}

const LIVE_STATS: DelegationRuntimeStats = {
  ...emptyRuntimeStats(STARTED_AT),
  tool_call_count: 12,
  edit_tool_call_count: 2,
  finished_at: FINISHED_AT,
  touched_files: [
    {
      path: "src/a.ts",
      outside_workspace: false,
      additions: 3,
      deletions: 1,
    },
  ],
  line_counts_complete: true,
  additions: 3,
  deletions: 1,
}

const RUNNING_SUMMARY_STATS: DelegationRuntimeStats = {
  ...emptyRuntimeStats(STARTED_AT),
  tool_call_count: 99,
  edit_tool_call_count: 9,
}

function binding(
  overrides: Partial<DelegationBinding> = {}
): DelegationBinding {
  return {
    parentConnectionId: "p1",
    parentToolUseId: "pt-1",
    childConnectionId: "c1",
    childConversationId: 99,
    agentType: "codex",
    status: "running",
    task: null,
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

function meta(overrides: Partial<ParsedMeta> = {}): ParsedMeta {
  return {
    status: "running",
    task: null,
    taskId: "task-meta",
    childConnectionId: "c-meta",
    childConversationId: 42,
    errorCode: null,
    startedAt: STARTED_AT,
    finishedAt: null,
    runtimeStats: emptyRuntimeStats(STARTED_AT),
    attentionRequest: null,
    textPreview: null,
    generation: null,
    syntheticHistorical: false,
    ...overrides,
  }
}

function projection(
  overrides: Partial<ChildCardProjection> = {}
): ChildCardProjection {
  return {
    childConversationId: 99,
    title: "Fix login flow",
    taskId: "task-summary",
    taskStatus: "running",
    errorCode: null,
    startedAt: STARTED_AT,
    finishedAt: null,
    runtimeStats: RUNNING_SUMMARY_STATS,
    attentionRequest: ATTENTION,
    isTerminal: false,
    ...overrides,
  }
}

function runSnapshot(
  overrides: Partial<DelegationRunSnapshot> = {}
): DelegationRunSnapshot {
  return {
    task_id: "task-1",
    root_task_id: "task-1",
    previous_task_id: null,
    generation: 1,
    parent_tool_use_id: "pt-1",
    child_conversation_id: 123,
    agent_type: "grok",
    profile_id: null,
    task_preview: "durable task",
    status: "running",
    error_code: null,
    started_at: STARTED_AT,
    finished_at: null,
    runtime_stats: RUNNING_SUMMARY_STATS,
    card_summary: null,
    child_turn_anchor: null,
    replaced_task_id: null,
    replacement_reason: null,
    ...overrides,
  }
}

const PARSED_INPUT = parseInput(
  JSON.stringify({
    agent_type: "codex",
    task: "raw task text",
    profile_label: "Codex Profile",
  })
)

function build(
  overrides: Partial<Parameters<typeof buildDelegationCardModel>[0]> = {}
) {
  return buildDelegationCardModel({
    parsedInput: PARSED_INPUT,
    parsedMeta: null,
    toolOutput: null,
    binding: undefined,
    childProjection: null,
    childAwaitingPermission: false,
    state: "input-available",
    errorText: null,
    nowMs: NOW_MS,
    ...overrides,
  })
}

function sourceWithStats(input: {
  parentToolUseId: string
  taskId: string
  status: "running" | "completed" | "failed"
  toolCallCount: number
  startedAt: string
  finishedAt?: string | null
  errorCode?: string | null
}): DelegationCardSource {
  return {
    parentToolUseId: input.parentToolUseId,
    parentConversationId: 10,
    input: JSON.stringify({
      agent_type: "codex",
      task: "work-unit task",
      work_unit_key: "unit-a",
    }),
    meta: {
      "codeg.delegation": {
        status: input.status,
        task_id: input.taskId,
        child_conversation_id: 99,
        started_at: input.startedAt,
        finished_at: input.finishedAt ?? null,
        error_code: input.errorCode ?? null,
        runtime_stats: {
          ...emptyRuntimeStats(input.startedAt),
          finished_at: input.finishedAt ?? null,
          tool_call_count: input.toolCallCount,
        },
      },
    },
  }
}

describe("buildDelegationCardModel — merge precedence", () => {
  it("matching completed snapshot replaces every stale running binding field", () => {
    const terminalSummary: CardSummary = {
      kind: "implementation",
      phase: "implementation",
      status: "done",
      summary: "Durable run completed.",
    }
    const model = build({
      parsedInput: parseInput(null),
      binding: binding({
        taskId: "task-1",
        status: "running",
        agentType: "codex",
        task: "stale live task",
        childConnectionId: "stale-connection",
        childConversationId: 99,
        runtimeStats: RUNNING_SUMMARY_STATS,
        attentionRequest: ATTENTION,
        errorCode: "stale-running-error",
        completedDurationMs: 45_000,
        cardSummary: null,
      }),
      runSnapshot: runSnapshot({
        task_id: "task-1",
        generation: 4,
        status: "completed",
        agent_type: "grok",
        task_preview: "durable task",
        child_conversation_id: 123,
        runtime_stats: LIVE_STATS,
        finished_at: FINISHED_AT,
        card_summary: terminalSummary,
      }),
    })

    expect(model).toMatchObject({
      lifecycleStatus: "ok",
      status: "ok",
      brokerTaskId: "task-1",
      generation: 4,
      agentType: "grok",
      task: "durable task",
      childConversationId: 123,
      childConnectionId: null,
      runtimeStats: LIVE_STATS,
      finishedAt: FINISHED_AT,
      attentionRequest: null,
      completedDurationMs: null,
      errorCode: undefined,
      cardSummary: terminalSummary,
    })
    expect(model.toolCallCount).toBe(12)
    expect(isTickerEligible(model)).toBe(false)
  })

  it("matching failed snapshot replaces stale running live meta as an error", () => {
    const model = build({
      parsedInput: parseInput(null),
      parsedMeta: meta({
        taskId: "task-1",
        status: "running",
        task: "stale meta task",
        childConnectionId: "stale-meta-connection",
        childConversationId: 42,
        runtimeStats: RUNNING_SUMMARY_STATS,
        attentionRequest: ATTENTION,
        errorCode: "stale-meta-error",
      }),
      runSnapshot: runSnapshot({
        task_id: "task-1",
        generation: 2,
        status: "failed",
        error_code: "durable_child_failed",
        task_preview: "durable failed task",
        child_conversation_id: 123,
        runtime_stats: LIVE_STATS,
        finished_at: FINISHED_AT,
      }),
    })

    expect(model).toMatchObject({
      lifecycleStatus: "err",
      status: "err",
      brokerTaskId: "task-1",
      generation: 2,
      task: "durable failed task",
      childConversationId: 123,
      childConnectionId: null,
      runtimeStats: LIVE_STATS,
      finishedAt: FINISHED_AT,
      attentionRequest: null,
      errorCode: "durable_child_failed",
    })
    expect(isTickerEligible(model)).toBe(false)
  })

  it("mismatched terminal snapshot cannot change a running binding", () => {
    const model = build({
      parsedInput: parseInput(null),
      binding: binding({
        taskId: "task-1",
        status: "running",
        task: "live task",
        runtimeStats: RUNNING_SUMMARY_STATS,
      }),
      runSnapshot: runSnapshot({
        task_id: "task-other",
        generation: 9,
        status: "completed",
        runtime_stats: LIVE_STATS,
        finished_at: FINISHED_AT,
        card_summary: {
          kind: "implementation",
          phase: "implementation",
          status: "done",
          summary: "Wrong run.",
        },
      }),
    })

    expect(model.lifecycleStatus).toBe("running")
    expect(model.status).toBe("active")
    expect(model.brokerTaskId).toBe("task-1")
    expect(model.generation).toBeNull()
    expect(model.task).toBe("live task")
    expect(model.agentType).toBe("codex")
    expect(model.childConnectionId).toBe("c1")
    expect(model.runtimeStats).toEqual(RUNNING_SUMMARY_STATS)
    expect(model.finishedAt).toBeNull()
    expect(model.cardSummary).toBeNull()
  })

  it("terminal snapshot fails closed when live meta has no task id", () => {
    const model = build({
      parsedInput: parseInput(null),
      parsedMeta: meta({
        taskId: null,
        status: "running",
        task: "identity-less meta task",
        runtimeStats: RUNNING_SUMMARY_STATS,
      }),
      runSnapshot: runSnapshot({
        task_id: "task-1",
        status: "completed",
        runtime_stats: LIVE_STATS,
        finished_at: FINISHED_AT,
      }),
    })

    expect(model.lifecycleStatus).toBe("running")
    expect(model.brokerTaskId).toBeNull()
    expect(model.task).toBe("identity-less meta task")
    expect(model.childConnectionId).toBe("c-meta")
    expect(model.runtimeStats).toEqual(RUNNING_SUMMARY_STATS)
    expect(model.finishedAt).toBeNull()
  })

  it("terminal live meta outranks a running binding and running snapshot", () => {
    const model = build({
      parsedInput: parseInput(null),
      binding: binding({
        taskId: "task-1",
        status: "running",
        task: "stale binding task",
        runtimeStats: RUNNING_SUMMARY_STATS,
      }),
      parsedMeta: meta({
        taskId: "task-1",
        status: "err",
        task: "terminal meta task",
        errorCode: "live_meta_failed",
        finishedAt: FINISHED_AT,
        runtimeStats: LIVE_STATS,
        attentionRequest: null,
      }),
      runSnapshot: runSnapshot({
        task_id: "task-1",
        status: "running",
        runtime_stats: RUNNING_SUMMARY_STATS,
      }),
    })

    expect(model.lifecycleStatus).toBe("err")
    expect(model.status).toBe("err")
    expect(model.task).toBe("terminal meta task")
    expect(model.errorCode).toBe("live_meta_failed")
    expect(model.childConnectionId).toBe("c-meta")
    expect(model.runtimeStats).toEqual(LIVE_STATS)
    expect(model.finishedAt).toBe(FINISHED_AT)
  })

  it("terminal live binding outranks a lower running snapshot", () => {
    const liveSummary: CardSummary = {
      kind: "review",
      verdict: "approve",
      critical: 0,
      important: 0,
      minor: 0,
      summary: "Live completion wins.",
    }
    const model = build({
      parsedInput: parseInput(null),
      binding: binding({
        taskId: "task-1",
        status: "ok",
        runtimeStats: LIVE_STATS,
        finishedAt: FINISHED_AT,
        attentionRequest: null,
        cardSummary: liveSummary,
      }),
      runSnapshot: runSnapshot({
        task_id: "task-1",
        status: "running",
        error_code: null,
        runtime_stats: RUNNING_SUMMARY_STATS,
        card_summary: {
          kind: "review",
          verdict: "block",
          critical: 1,
          important: 0,
          minor: 0,
          summary: "Lower running data.",
        },
      }),
    })

    expect(model.lifecycleStatus).toBe("ok")
    expect(model.status).toBe("ok")
    expect(model.errorCode).toBeUndefined()
    expect(model.runtimeStats).toEqual(LIVE_STATS)
    expect(model.cardSummary).toEqual(liveSummary)
  })

  it("terminal live locks lifecycle and stats over a running summary", () => {
    const model = build({
      binding: binding({
        status: "ok",
        runtimeStats: LIVE_STATS,
        finishedAt: FINISHED_AT,
        completedDurationMs: 90_000,
        attentionRequest: null,
      }),
      childProjection: projection({
        taskStatus: "running",
        isTerminal: false,
        runtimeStats: RUNNING_SUMMARY_STATS,
        attentionRequest: ATTENTION,
        finishedAt: null,
      }),
    })

    expect(model.lifecycleStatus).toBe("ok")
    expect(model.runtimeStats).toEqual(LIVE_STATS)
    expect(model.toolCallCount).toBe(12)
    expect(model.finishedAt).toBe(FINISHED_AT)
    // Live null attention is an authoritative clear — no stale summary request.
    expect(model.attentionRequest).toBeNull()
    expect(model.editRollup).toEqual({
      mode: "files",
      fileCount: 1,
      fileCountTruncated: false,
      additions: 3,
      deletions: 1,
      showLineTotals: true,
    })
  })

  it("terminal meta locks lifecycle and stats over a running summary", () => {
    const terminalMetaStats: DelegationRuntimeStats = {
      ...LIVE_STATS,
      tool_call_count: 7,
    }
    const model = build({
      parsedMeta: meta({
        status: "ok",
        runtimeStats: terminalMetaStats,
        finishedAt: FINISHED_AT,
        attentionRequest: null,
      }),
      childProjection: projection({
        taskStatus: "running",
        isTerminal: false,
        runtimeStats: RUNNING_SUMMARY_STATS,
        attentionRequest: ATTENTION,
      }),
    })

    expect(model.lifecycleStatus).toBe("ok")
    expect(model.runtimeStats).toEqual(terminalMetaStats)
    expect(model.toolCallCount).toBe(7)
    expect(model.attentionRequest).toBeNull()
    expect(model.finishedAt).toBe(FINISHED_AT)
  })

  it("uses an immutable run snapshot when terminal meta lacks runtime fields", () => {
    const snapshotStats: DelegationRuntimeStats = {
      ...emptyRuntimeStats(STARTED_AT),
      finished_at: "2026-07-19T00:00:45.000Z",
      tool_call_count: 2,
      edit_tool_call_count: 1,
      touched_files: [],
      line_counts_complete: false,
    }
    const model = build({
      parsedMeta: meta({
        status: "ok",
        taskId: "run-1",
        runtimeStats: null,
        finishedAt: null,
      }),
      runSnapshot: {
        task_id: "run-1",
        root_task_id: "run-1",
        previous_task_id: null,
        generation: 1,
        parent_tool_use_id: "pt-1",
        child_conversation_id: 99,
        agent_type: "codex",
        profile_id: null,
        task_preview: "first review",
        status: "completed",
        error_code: null,
        started_at: STARTED_AT,
        finished_at: "2026-07-19T00:00:45.000Z",
        runtime_stats: snapshotStats,
        card_summary: null,
        child_turn_anchor: null,
        replaced_task_id: null,
        replacement_reason: null,
      },
      childProjection: projection({
        taskId: "run-2",
        taskStatus: "completed",
        isTerminal: true,
        runtimeStats: LIVE_STATS,
        finishedAt: FINISHED_AT,
      }),
    })

    expect(model.runtimeStats).toEqual(snapshotStats)
    expect(model.finishedAt).toBe("2026-07-19T00:00:45.000Z")
  })

  it("uses historical generation immediately and lets its run snapshot close stale lifecycle", () => {
    const historicalMeta = meta({
      status: "running",
      taskId: "run-3",
      generation: 3,
      syntheticHistorical: true,
      finishedAt: null,
      runtimeStats: null,
    })
    const beforeHydration = build({
      parsedMeta: historicalMeta,
      runSnapshot: null,
    })
    expect(beforeHydration.generation).toBe(3)
    expect(beforeHydration.lifecycleStatus).toBe("running")

    const afterHydration = build({
      parsedMeta: historicalMeta,
      runSnapshot: {
        task_id: "run-3",
        root_task_id: "run-1",
        previous_task_id: "run-2",
        generation: 3,
        parent_tool_use_id: "pt-3",
        child_conversation_id: 99,
        agent_type: "grok",
        profile_id: null,
        task_preview: "third review",
        status: "completed",
        error_code: null,
        started_at: STARTED_AT,
        finished_at: FINISHED_AT,
        runtime_stats: LIVE_STATS,
        card_summary: null,
        child_turn_anchor: null,
        replaced_task_id: null,
        replacement_reason: null,
      },
    })
    expect(afterHydration.generation).toBe(3)
    expect(afterHydration.lifecycleStatus).toBe("ok")
    expect(afterHydration.status).toBe("ok")
    expect(afterHydration.finishedAt).toBe(FINISHED_AT)
    expect(afterHydration.runtimeStats).toEqual(LIVE_STATS)
  })

  it("preserves matching historical attention across snapshot hydration", () => {
    const runAttention = { ...ATTENTION, task_id: "run-3" }
    const historicalMeta = meta({
      status: "running",
      taskId: "run-3",
      generation: 3,
      syntheticHistorical: true,
      attentionRequest: null,
    })
    const matchingProjection = projection({
      taskId: "run-3",
      attentionRequest: runAttention,
    })
    const snapshot = {
      task_id: "run-3",
      root_task_id: "run-1",
      previous_task_id: "run-2",
      generation: 3,
      parent_tool_use_id: "pt-3",
      child_conversation_id: 99,
      agent_type: "grok" as const,
      profile_id: null,
      task_preview: "third review",
      status: "running" as const,
      error_code: null,
      started_at: STARTED_AT,
      finished_at: null,
      runtime_stats: null,
      card_summary: null,
      child_turn_anchor: null,
      replaced_task_id: null,
      replacement_reason: null,
    }

    expect(
      build({
        parsedMeta: historicalMeta,
        childProjection: matchingProjection,
      }).attentionRequest
    ).toEqual(runAttention)
    expect(
      build({
        parsedMeta: historicalMeta,
        runSnapshot: snapshot,
        childProjection: matchingProjection,
      }).attentionRequest
    ).toEqual(runAttention)
  })

  it("rejects historical attention whose task id does not match the run", () => {
    const model = build({
      parsedMeta: meta({
        taskId: "run-2",
        syntheticHistorical: true,
        attentionRequest: null,
      }),
      childProjection: projection({
        taskId: "run-2",
        attentionRequest: { ...ATTENTION, task_id: "run-3" },
      }),
    })

    expect(model.attentionRequest).toBeNull()
  })

  it("does not adopt a later child projection for an older terminal task without a snapshot", () => {
    const model = build({
      parsedMeta: meta({
        status: "ok",
        taskId: "run-1",
        runtimeStats: null,
        finishedAt: null,
      }),
      childProjection: projection({
        taskId: "run-2",
        taskStatus: "completed",
        isTerminal: true,
        runtimeStats: LIVE_STATS,
        finishedAt: FINISHED_AT,
        title: "Shared child session",
      }),
    })

    expect(model.brokerTaskId).toBe("run-1")
    expect(model.runtimeStats).toBeNull()
    expect(model.finishedAt).toBeNull()
    // Shared session chrome may still use the child title.
    expect(model.conversationTitle).toBe("Shared child session")
  })

  it("fails closed when known task_id meets a null-taskId child projection", () => {
    // Child rows can briefly (or permanently) lack delegation_call_id. That
    // must not reopen/mutate a terminal card that already knows its task_id.
    const model = build({
      parsedMeta: meta({
        status: "ok",
        taskId: "run-1",
        runtimeStats: null,
        finishedAt: null,
      }),
      childProjection: projection({
        taskId: null,
        taskStatus: "completed",
        isTerminal: true,
        runtimeStats: LIVE_STATS,
        finishedAt: FINISHED_AT,
        title: "Shared child session",
      }),
    })

    expect(model.brokerTaskId).toBe("run-1")
    expect(model.lifecycleStatus).toBe("ok")
    expect(model.runtimeStats).toBeNull()
    expect(model.finishedAt).toBeNull()
    expect(model.toolCallCount).toBeNull()
    // Title is session-scoped, not run-scoped — still allowed.
    expect(model.conversationTitle).toBe("Shared child session")
  })

  it("live attentionRequest: null clears stale summary attention", () => {
    const model = build({
      binding: binding({
        status: "running",
        attentionRequest: null,
      }),
      childProjection: projection({
        attentionRequest: ATTENTION,
      }),
    })
    expect(model.attentionRequest).toBeNull()
  })

  it("meta attentionRequest: null clears stale summary attention", () => {
    const model = build({
      parsedMeta: meta({
        status: "running",
        attentionRequest: null,
      }),
      childProjection: projection({
        attentionRequest: ATTENTION,
      }),
    })
    expect(model.attentionRequest).toBeNull()
  })

  it("falls through to summary attention only when live/meta omit attention", () => {
    // No binding/meta → summary attention is used for cold recovery.
    const model = build({
      childProjection: projection({
        attentionRequest: ATTENTION,
      }),
      toolOutput: { kind: "ack", childConversationId: 99 },
    })
    expect(model.attentionRequest).toEqual(ATTENTION)
  })

  it("prefers live completedDurationMs over tool-output durationMs", () => {
    const toolOutput: ParsedToolOutput = {
      kind: "outcome",
      text: "done",
      isError: false,
      childConversationId: 99,
      durationMs: 12_000,
      errorCode: null,
    }
    const model = build({
      binding: binding({
        status: "ok",
        finishedAt: FINISHED_AT,
        completedDurationMs: 90_000,
        runtimeStats: LIVE_STATS,
      }),
      toolOutput,
    })
    expect(model.completedDurationMs).toBe(90_000)
    // Terminal elapsed prefers finished - started over completedDurationMs.
    expect(model.elapsedMs).toBe(90_000)
  })

  it("uses tool-output durationMs when live completion duration is absent", () => {
    const toolOutput: ParsedToolOutput = {
      kind: "outcome",
      text: "done",
      isError: false,
      childConversationId: 99,
      durationMs: 45_000,
      errorCode: null,
    }
    const model = build({
      parsedMeta: meta({
        status: "ok",
        startedAt: null,
        finishedAt: null,
        runtimeStats: null,
      }),
      toolOutput,
    })
    expect(model.completedDurationMs).toBe(45_000)
    expect(model.elapsedMs).toBe(45_000)
  })

  it("omits completedDurationMs when neither live nor tool-output provide it", () => {
    const model = build({
      parsedMeta: meta({
        status: "ok",
        startedAt: STARTED_AT,
        finishedAt: FINISHED_AT,
        runtimeStats: null,
      }),
      toolOutput: {
        kind: "outcome",
        text: "done",
        isError: false,
        childConversationId: 42,
        durationMs: null,
        errorCode: null,
      },
    })
    expect(model.completedDurationMs).toBeNull()
    expect(model.elapsedMs).toBe(90_000)
  })
})

describe("buildDelegationCardModel — lifecycle vs badge / ticker", () => {
  it.each([
    ["active", "active"],
    ["stalled", "stalled"],
    ["waiting_input", "waiting_input"],
  ] as const)(
    "badge %s keeps lifecycleStatus running (ticker eligible)",
    (observation, badge) => {
      const model = build({
        binding: binding({
          status: "running",
          observation,
          startedAt: STARTED_AT,
        }),
      })
      expect(model.status).toBe(badge)
      expect(model.lifecycleStatus).toBe("running")
      expect(isTickerEligible(model)).toBe(true)
      expect(model.elapsedMs).toBe(NOW_MS - Date.parse(STARTED_AT))
    }
  )

  it("permission waiting badge still has lifecycleStatus running", () => {
    const model = build({
      binding: binding({ status: "running", observation: "active" }),
      childAwaitingPermission: true,
    })
    expect(model.status).toBe("waiting")
    expect(model.lifecycleStatus).toBe("running")
    expect(isTickerEligible(model)).toBe(true)
  })

  it("terminal lifecycle is never ticker-eligible", () => {
    const ok = build({
      binding: binding({
        status: "ok",
        startedAt: STARTED_AT,
        finishedAt: FINISHED_AT,
      }),
    })
    const err = build({
      binding: binding({
        status: "err",
        errorCode: "failed",
        startedAt: STARTED_AT,
        finishedAt: FINISHED_AT,
      }),
    })
    expect(isTickerEligible(ok)).toBe(false)
    expect(isTickerEligible(err)).toBe(false)
  })

  it("running without valid startedAt is not ticker-eligible", () => {
    const model = build({
      binding: binding({ status: "running", startedAt: "not-a-date" }),
    })
    expect(model.lifecycleStatus).toBe("running")
    expect(isTickerEligible(model)).toBe(false)
    expect(model.elapsedMs).toBeNull()
  })
})

describe("buildDelegationCardModel — synthetic / cold path", () => {
  it("keeps an uncorrelated failed continuation detached from the old child", () => {
    const model = build({
      parsedMeta: meta({
        status: "err",
        taskId: null,
        childConversationId: 77,
        childConnectionId: "old-child-connection",
        startedAt: null,
        runtimeStats: null,
      }),
      toolOutput: {
        kind: "outcome",
        text: "Continuation failed before a new run was reserved.",
        isError: true,
        childConversationId: 77,
        durationMs: null,
        errorCode: "continuation_not_resumable",
      },
      state: "output-error",
      errorText: "Continuation failed before a new run was reserved.",
      childProjection: projection({
        childConversationId: 77,
        taskId: "run-1",
        taskStatus: "completed",
        isTerminal: true,
        finishedAt: FINISHED_AT,
        runtimeStats: LIVE_STATS,
        title: "Old child session",
      }),
    })

    expect(model.lifecycleStatus).toBe("err")
    expect(model.status).toBe("err")
    expect(model.errorCode).toBe("continuation_not_resumable")
    expect(model.childConversationId).toBeNull()
    expect(model.childConnectionId).toBeNull()
    expect(model.conversationTitle).toBeNull()
    expect(model.runtimeStats).toBeNull()
  })

  it("ack + terminal projection aligns badge and lifecycle (no split brain)", () => {
    const model = build({
      toolOutput: { kind: "ack", childConversationId: 77 },
      childProjection: projection({
        childConversationId: 77,
        taskStatus: "completed",
        isTerminal: true,
        finishedAt: FINISHED_AT,
        runtimeStats: LIVE_STATS,
        attentionRequest: null,
        errorCode: null,
      }),
    })
    expect(model.lifecycleStatus).toBe("ok")
    expect(model.status).toBe("ok")
    expect(isTickerEligible(model)).toBe(false)
    expect(model.runtimeStats).toEqual(LIVE_STATS)
  })

  it("terminal tool outcome beats stale running projection", () => {
    const model = build({
      toolOutput: {
        kind: "outcome",
        text: "",
        isError: false,
        childConversationId: 77,
        durationMs: 45_000,
        errorCode: null,
      },
      state: "output-available",
      childProjection: projection({
        childConversationId: 77,
        taskStatus: "running",
        isTerminal: false,
        runtimeStats: RUNNING_SUMMARY_STATS,
        finishedAt: null,
      }),
    })
    expect(model.lifecycleStatus).toBe("ok")
    expect(model.status).toBe("ok")
    expect(isTickerEligible(model)).toBe(false)
    // Running lower summary stats must not be adopted under terminal tool.
    // pickRuntimeStats with no binding/meta still returns projection stats —
    // that is intentional fill when higher has no stats object. Lifecycle is
    // what locks ticker/elapsed.
    expect(model.completedDurationMs).toBe(45_000)
  })

  it("failed projection supplies errorCode on cold recovery", () => {
    const model = build({
      toolOutput: { kind: "ack", childConversationId: 77 },
      childProjection: projection({
        childConversationId: 77,
        taskStatus: "failed",
        isTerminal: true,
        errorCode: "child_failed",
        finishedAt: FINISHED_AT,
        runtimeStats: null,
        attentionRequest: null,
      }),
    })
    expect(model.lifecycleStatus).toBe("err")
    expect(model.status).toBe("err")
    expect(model.errorCode).toBe("child_failed")
  })

  it("correlation failure surfaces tool errorCode without a run snapshot", () => {
    const model = build({
      runSnapshot: null,
      childProjection: null,
      binding: undefined,
      parsedMeta: null,
      state: "output-error",
      errorText: null,
      toolOutput: {
        kind: "outcome",
        text: "Parent tool call could not be correlated. Not unresumable.",
        isError: true,
        childConversationId: null,
        durationMs: null,
        errorCode: "delegation_correlation_missing",
      },
    })
    expect(model.hasModel).toBe(true)
    expect(model.lifecycleStatus).toBe("err")
    expect(model.status).toBe("err")
    expect(model.errorCode).toBe("delegation_correlation_missing")
    expect(model.childConversationId).toBeNull()
    expect(model.brokerTaskId).toBeNull()
    expect(model.runtimeStats).toBeNull()
    // Must not be remapped to spawn/unresumable labels at the model layer.
    expect(model.errorCode).not.toBe("unresumable")
    expect(model.errorCode).not.toBe("spawn_failed")
  })

  it.each([
    "delegation_correlation_timeout",
    "delegation_correlation_ambiguous",
    "delegation_correlation_conflict",
    "provisional_admission_rejected",
    "provisional_terminalization_failed",
    "provisional_cleanup_failed",
  ] as const)("forwards wire error code %s from tool outcome", (code) => {
    const model = build({
      runSnapshot: null,
      childProjection: null,
      binding: undefined,
      parsedMeta: null,
      state: "output-error",
      toolOutput: {
        kind: "outcome",
        text: code,
        isError: true,
        childConversationId: null,
        durationMs: null,
        errorCode: code,
      },
    })
    expect(model.errorCode).toBe(code)
    expect(model.lifecycleStatus).toBe("err")
  })

  it("ack-only (no binding/meta) fabricates neither stats nor attention zeros", () => {
    const model = build({
      toolOutput: { kind: "ack", childConversationId: 77 },
      childProjection: null,
    })

    expect(model.lifecycleStatus).toBe("running")
    expect(model.status).toBe("running")
    expect(model.runtimeStats).toBeNull()
    expect(model.toolCallCount).toBeNull()
    expect(model.attentionRequest).toBeNull()
    expect(model.editRollup).toEqual({ mode: "omit" })
    expect(model.elapsedMs).toBeNull()
    expect(model.startedAt).toBeNull()
    expect(model.completedDurationMs).toBeNull()
    expect(model.conversationTitle).toBeNull()
    // Secondary falls through to task until title hydrates.
    expect(model.displaySecondary).toBe("raw task text")
    expect(model.childConversationId).toBe(77)
    expect(model.hasModel).toBe(true)
  })

  it("title appears only after child projection hydrate", () => {
    const before = build({
      toolOutput: { kind: "ack", childConversationId: 77 },
      childProjection: null,
    })
    expect(before.conversationTitle).toBeNull()
    expect(before.displaySecondary).toBe("raw task text")

    const after = build({
      toolOutput: { kind: "ack", childConversationId: 77 },
      childProjection: projection({
        childConversationId: 77,
        title: "  Seeded title  ",
        // Cold projection without stats must not invent zeros.
        runtimeStats: null,
        attentionRequest: null,
        taskStatus: "running",
        isTerminal: false,
      }),
    })
    expect(after.conversationTitle).toBe("  Seeded title  ")
    expect(after.displaySecondary).toBe("Seeded title")
    expect(after.runtimeStats).toBeNull()
    expect(after.toolCallCount).toBeNull()
    expect(after.attentionRequest).toBeNull()
  })

  it("does not treat emptyRuntimeStats absence as free for summary when higher source is terminal without stats", () => {
    // Terminal meta with null stats + running summary → do not adopt running summary stats.
    const model = build({
      parsedMeta: meta({
        status: "ok",
        runtimeStats: null,
        finishedAt: FINISHED_AT,
        attentionRequest: null,
      }),
      childProjection: projection({
        taskStatus: "running",
        isTerminal: false,
        runtimeStats: RUNNING_SUMMARY_STATS,
      }),
    })
    expect(model.lifecycleStatus).toBe("ok")
    expect(model.runtimeStats).toBeNull()
    expect(model.toolCallCount).toBeNull()
  })
})

describe("mergeDelegationWorkUnitModel", () => {
  const workStartedAt = "2026-07-27T00:00:00.000Z"
  const continuedAt = "2026-07-27T00:05:00.000Z"
  const nowMs = Date.parse("2026-07-27T00:06:00.000Z")

  it("sums per-run peaks and preserves the work-unit elapsed anchor", () => {
    const sources = [
      sourceWithStats({
        parentToolUseId: "pt-1",
        taskId: "run-1",
        status: "completed",
        toolCallCount: 5,
        startedAt: workStartedAt,
        finishedAt: continuedAt,
      }),
      sourceWithStats({
        parentToolUseId: "pt-2",
        taskId: "run-2",
        status: "running",
        toolCallCount: 2,
        startedAt: continuedAt,
      }),
    ]
    const current = build({
      parsedMeta: meta({
        status: "running",
        taskId: "run-2",
        startedAt: continuedAt,
        runtimeStats: {
          ...emptyRuntimeStats(continuedAt),
          tool_call_count: 2,
        },
      }),
      nowMs,
    })

    const merged = mergeDelegationWorkUnitModel({
      model: current,
      sources,
      stickyKey: "unit-a",
      nowMs,
      hasLiveBinding: true,
      explicitUserCancel: false,
    })

    expect(merged.toolCallCount).toBe(7)
    expect(merged.startedAt).toBe(workStartedAt)
    expect(merged.elapsedMs).toBe(360_000)
    expect(merged.showGeneratingSegment).toBe(true)
    expect(merged.stickyKey).toBe("unit-a")
  })

  it("keeps a recent recoverable orchestration error generating", () => {
    const source = sourceWithStats({
      parentToolUseId: "pt-1",
      taskId: "run-1",
      status: "failed",
      toolCallCount: 5,
      startedAt: workStartedAt,
      finishedAt: continuedAt,
      errorCode: "parent_turn_failed",
    })
    const current = build({
      parsedMeta: meta({
        status: "err",
        taskId: "run-1",
        errorCode: "parent_turn_failed",
        startedAt: workStartedAt,
        finishedAt: continuedAt,
        runtimeStats: {
          ...emptyRuntimeStats(workStartedAt),
          finished_at: continuedAt,
          tool_call_count: 5,
        },
      }),
      nowMs,
    })

    const merged = mergeDelegationWorkUnitModel({
      model: current,
      sources: [source],
      stickyKey: "unit-a",
      nowMs,
      hasLiveBinding: false,
      explicitUserCancel: false,
    })

    expect(merged.lifecycleStatus).toBe("running")
    expect(merged.status).toBe("running")
    expect(merged.errorCode).toBeUndefined()
    expect(merged.showGeneratingSegment).toBe(true)
  })

  it.each([
    ["completed", "ok", null, false],
    ["failed", "err", "user_cancelled", true],
  ] as const)(
    "does not show generating after %s",
    (sourceStatus, modelStatus, errorCode, explicitUserCancel) => {
      const source = sourceWithStats({
        parentToolUseId: "pt-1",
        taskId: "run-1",
        status: sourceStatus,
        toolCallCount: 5,
        startedAt: workStartedAt,
        finishedAt: continuedAt,
        errorCode,
      })
      const current = build({
        parsedMeta: meta({
          status: modelStatus,
          taskId: "run-1",
          errorCode,
          startedAt: workStartedAt,
          finishedAt: continuedAt,
          runtimeStats: {
            ...emptyRuntimeStats(workStartedAt),
            finished_at: continuedAt,
            tool_call_count: 5,
          },
        }),
        nowMs,
      })

      const merged = mergeDelegationWorkUnitModel({
        model: current,
        sources: [source],
        stickyKey: "unit-a",
        nowMs,
        hasLiveBinding: false,
        explicitUserCancel,
      })

      expect(merged.lifecycleStatus).toBe(modelStatus)
      expect(merged.showGeneratingSegment).toBe(false)
    }
  )
})

describe("buildDelegationCardModel — identity + secondary", () => {
  it("prefers binding identity and broker task id", () => {
    const model = build({
      binding: binding({
        taskId: "live-task",
        agentType: "claude_code",
        childConversationId: 11,
        childConnectionId: "conn-live",
      }),
      parsedMeta: meta({
        taskId: "meta-task",
        childConversationId: 22,
        childConnectionId: "conn-meta",
      }),
      childProjection: projection({
        taskId: "summary-task",
        title: "From summary",
      }),
    })
    expect(model.brokerTaskId).toBe("live-task")
    expect(model.childConversationId).toBe(11)
    expect(model.childConnectionId).toBe("conn-live")
    expect(model.agentType).toBe("claude_code")
    expect(model.conversationTitle).toBe("From summary")
    expect(model.displaySecondary).toBe("From summary")
    expect(model.agentDisplayLabel).toBe("Codex Profile")
    expect(model.task).toBe("raw task text")
  })

  it("hasModel is false without binding, agent, task, or meta", () => {
    const emptyInput = parseInput(null)
    const model = buildDelegationCardModel({
      parsedInput: emptyInput,
      parsedMeta: null,
      toolOutput: null,
      binding: undefined,
      childProjection: projection({ title: "orphan title only" }),
      childAwaitingPermission: false,
      nowMs: NOW_MS,
    })
    // Title alone must not force hasModel.
    expect(model.hasModel).toBe(false)
    expect(model.conversationTitle).toBe("orphan title only")
  })

  it("hasModel is true when meta alone is present", () => {
    const emptyInput = parseInput(null)
    const model = buildDelegationCardModel({
      parsedInput: emptyInput,
      parsedMeta: meta(),
      toolOutput: null,
      binding: undefined,
      childProjection: null,
      childAwaitingPermission: false,
      nowMs: NOW_MS,
    })
    expect(model.hasModel).toBe(true)
  })

  it("keeps independent terminal cards immutable when later runs share a child", () => {
    const firstSummary: CardSummary = {
      kind: "review",
      verdict: "approve",
      critical: 0,
      important: 0,
      minor: 0,
      summary: "First review is complete.",
    }
    const secondSummary: CardSummary = {
      kind: "implementation",
      phase: "fix",
      status: "done",
      summary: "Second run fixed the issue.",
    }
    const latestChildProjection = projection({
      taskId: "run-2",
      taskStatus: "completed",
      isTerminal: true,
      runtimeStats: LIVE_STATS,
      finishedAt: FINISHED_AT,
    })
    const firstExtra: Record<string, unknown> = {
      runSnapshot: {
        task_id: "run-1",
        child_conversation_id: 99,
        generation: 1,
        status: "completed",
        runtime_stats: emptyRuntimeStats(STARTED_AT),
        card_summary: firstSummary,
      },
    }
    const secondExtra: Record<string, unknown> = {
      runSnapshot: {
        task_id: "run-2",
        child_conversation_id: 99,
        generation: 2,
        status: "completed",
        runtime_stats: LIVE_STATS,
        card_summary: secondSummary,
      },
    }

    const first = build({
      childProjection: latestChildProjection,
      ...firstExtra,
    })
    const second = build({
      childProjection: latestChildProjection,
      ...secondExtra,
    })

    expect(first.brokerTaskId).toBe("run-1")
    expect(second.brokerTaskId).toBe("run-2")
    expect(
      (first as typeof first & { cardSummary?: CardSummary | null }).cardSummary
    ).toEqual(firstSummary)
    expect(
      (second as typeof second & { cardSummary?: CardSummary | null })
        .cardSummary
    ).toEqual(secondSummary)
  })

  it("prefers this run's snapshot summary over a later continue's live binding", () => {
    // Simulates what would happen if child-fallback incorrectly attached the
    // gen-2 live binding to a gen-1 card: after scoping, binding is dropped
    // and Critical/Important counts stay on this run's snapshot.
    const firstSummary: CardSummary = {
      kind: "review",
      verdict: "request_changes",
      critical: 2,
      important: 1,
      minor: 0,
      summary: "Mirror still diverges on path handling.",
    }
    const laterBinding = binding({
      parentToolUseId: "pt-2",
      taskId: "run-2",
      status: "running",
      task: "Continue fixing",
      cardSummary: {
        kind: "review",
        verdict: "approve",
        critical: 0,
        important: 0,
        minor: 0,
        summary: "Later run — must not paint over turn 1.",
      },
    })
    const scoped = scopeDelegationBindingForCard(laterBinding, "pt-1", "run-1")
    expect(scoped).toBeUndefined()

    const model = build({
      binding: scoped,
      parsedMeta: meta({
        status: "ok",
        taskId: "run-1",
        finishedAt: FINISHED_AT,
        runtimeStats: { ...LIVE_STATS, finished_at: FINISHED_AT },
      }),
      runSnapshot: {
        task_id: "run-1",
        root_task_id: "run-1",
        previous_task_id: null,
        generation: 1,
        parent_tool_use_id: "pt-1",
        child_conversation_id: 99,
        agent_type: "codex",
        profile_id: null,
        task_preview: "review turn 1",
        status: "completed",
        error_code: null,
        started_at: STARTED_AT,
        finished_at: FINISHED_AT,
        runtime_stats: emptyRuntimeStats(STARTED_AT),
        card_summary: firstSummary,
        child_turn_anchor: null,
        replaced_task_id: null,
        replacement_reason: null,
      },
    })

    expect(model.cardSummary).toEqual(firstSummary)
    expect(model.cardSummary).toMatchObject({
      kind: "review",
      critical: 2,
      important: 1,
      summary: "Mirror still diverges on path handling.",
    })
  })
})

describe("scopeDelegationBindingForCard", () => {
  it("accepts only the binding for this parent tool-use id and task id", () => {
    const own = binding({ parentToolUseId: "pt-1", taskId: "run-1" })
    expect(scopeDelegationBindingForCard(own, "pt-1", "run-1")).toBe(own)
    expect(scopeDelegationBindingForCard(own, "pt-1", null)).toBe(own)
    expect(
      scopeDelegationBindingForCard(own, "pt-other", "run-1")
    ).toBeUndefined()
    expect(
      scopeDelegationBindingForCard(own, "pt-1", "run-other")
    ).toBeUndefined()
  })
})
