import { describe, expect, it } from "vitest"

import {
  buildDelegationCardModel,
  isTickerEligible,
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
    taskId: "task-meta",
    childConnectionId: "c-meta",
    childConversationId: 42,
    errorCode: null,
    startedAt: STARTED_AT,
    finishedAt: null,
    runtimeStats: emptyRuntimeStats(STARTED_AT),
    attentionRequest: null,
    textPreview: null,
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

describe("buildDelegationCardModel — merge precedence", () => {
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
})
