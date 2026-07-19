import { describe, expect, it } from "vitest"

import {
  buildEditRollupViewModel,
  computeDelegationElapsedMs,
  formatDelegationDisplaySecondary,
  parseDelegationMeta,
  parseInput,
  parseToolOutput,
  resolveDelegationStatus,
} from "@/lib/delegation-card"
import type { DelegationBinding } from "@/contexts/delegation-context"
import {
  AGENT_LABELS,
  ALL_AGENT_TYPES,
  emptyRuntimeStats,
  type DelegationRuntimeStats,
} from "@/lib/types"

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
    startedAt: "2026-07-19T00:00:00.000Z",
    runtimeStats: emptyRuntimeStats("2026-07-19T00:00:00.000Z"),
    observation: "active",
    ...overrides,
  }
}

describe("parseDelegationMeta", () => {
  it("forwards full projection fields", () => {
    const runtimeStats = {
      ...emptyRuntimeStats("2026-07-19T10:00:00.000Z"),
      finished_at: "2026-07-19T10:05:00.000Z",
      tool_call_count: 3,
      edit_tool_call_count: 1,
      touched_files: [
        {
          path: "src/lib/foo.ts",
          outside_workspace: false,
          additions: 4,
          deletions: 1,
        },
      ],
      touched_files_truncated: false,
      additions: 4,
      deletions: 1,
      line_counts_complete: true,
    }
    const attentionRequest = {
      request_id: "req-1",
      task_id: "task-abc",
      message: "Need parent decision",
      created_at: "2026-07-19T10:02:00.000Z",
    }

    const parsed = parseDelegationMeta({
      "codeg.delegation": {
        status: "completed",
        task_id: "task-abc",
        child_connection_id: "child-conn",
        child_conversation_id: 42,
        error_code: null,
        text_preview: "done preview",
        started_at: "2026-07-19T10:00:00.000Z",
        finished_at: "2026-07-19T10:05:00.000Z",
        runtime_stats: runtimeStats,
        attention_request: attentionRequest,
      },
    })

    expect(parsed).toEqual({
      status: "ok",
      taskId: "task-abc",
      childConnectionId: "child-conn",
      childConversationId: 42,
      errorCode: null,
      startedAt: "2026-07-19T10:00:00.000Z",
      finishedAt: "2026-07-19T10:05:00.000Z",
      runtimeStats,
      attentionRequest,
      textPreview: "done preview",
    })
  })

  it("returns null runtimeStats when shape invalid", () => {
    expect(
      parseDelegationMeta({
        "codeg.delegation": {
          status: "running",
          child_conversation_id: 1,
          runtime_stats: { tool_call_count: "nope" },
        },
      })?.runtimeStats
    ).toBeNull()
  })

  it("returns null attentionRequest when shape invalid", () => {
    expect(
      parseDelegationMeta({
        "codeg.delegation": {
          status: "running",
          child_conversation_id: 1,
          attention_request: { request_id: 123 },
        },
      })?.attentionRequest
    ).toBeNull()
  })

  it("normalizes empty task_id to null", () => {
    expect(
      parseDelegationMeta({
        "codeg.delegation": {
          status: "running",
          task_id: "",
          child_conversation_id: 1,
        },
      })?.taskId
    ).toBeNull()
  })

  it("omits absent nested projection fields as null", () => {
    const parsed = parseDelegationMeta({
      "codeg.delegation": {
        status: "running",
        child_conversation_id: 7,
      },
    })
    expect(parsed).toMatchObject({
      status: "running",
      taskId: null,
      childConnectionId: null,
      childConversationId: 7,
      errorCode: null,
      startedAt: null,
      finishedAt: null,
      runtimeStats: null,
      attentionRequest: null,
      textPreview: null,
    })
  })
})

describe("resolveDelegationStatus — live binding observation", () => {
  it.each([
    ["active", "active"],
    ["waiting_input", "waiting_input"],
    ["stalled", "stalled"],
  ] as const)(
    "maps running/%s binding observation to card status %s",
    (observation, expected) => {
      expect(
        resolveDelegationStatus({
          binding: binding({ status: "running", observation }),
          parsedMeta: null,
          toolOutput: null,
          state: "input-available",
          errorText: null,
          childAwaitingPermission: false,
        })
      ).toBe(expected)
    }
  )

  it("keeps ok/err terminal statuses and ignores observation", () => {
    expect(
      resolveDelegationStatus({
        binding: binding({ status: "ok", observation: "stalled" }),
        parsedMeta: null,
        toolOutput: null,
        state: "output-available",
        errorText: null,
        childAwaitingPermission: false,
      })
    ).toBe("ok")
    expect(
      resolveDelegationStatus({
        binding: binding({
          status: "err",
          observation: null,
          errorCode: "timeout",
        }),
        parsedMeta: null,
        toolOutput: null,
        state: "output-error",
        errorText: "failed",
        childAwaitingPermission: false,
      })
    ).toBe("err")
  })

  it("prefers permission waiting over observation", () => {
    expect(
      resolveDelegationStatus({
        binding: binding({
          status: "running",
          observation: "stalled",
        }),
        parsedMeta: null,
        toolOutput: null,
        state: "input-available",
        errorText: null,
        childAwaitingPermission: true,
      })
    ).toBe("waiting")
  })

  it("falls back to plain running when observation is absent", () => {
    expect(
      resolveDelegationStatus({
        binding: binding({ observation: undefined }),
        parsedMeta: null,
        toolOutput: null,
        state: "input-available",
        errorText: null,
        childAwaitingPermission: false,
      })
    ).toBe("running")
  })

  it("terminal childTaskStatus wins over parent ack (cold recovery)", () => {
    expect(
      resolveDelegationStatus({
        binding: undefined,
        parsedMeta: null,
        toolOutput: { kind: "ack", childConversationId: 1 },
        state: "output-available",
        errorText: null,
        childAwaitingPermission: false,
        childTaskStatus: "completed",
      })
    ).toBe("ok")
    expect(
      resolveDelegationStatus({
        binding: undefined,
        parsedMeta: null,
        toolOutput: { kind: "ack", childConversationId: 1 },
        state: "output-available",
        errorText: null,
        childAwaitingPermission: false,
        childTaskStatus: "failed",
      })
    ).toBe("err")
  })

  it("terminal tool outcome wins over running childTaskStatus", () => {
    expect(
      resolveDelegationStatus({
        binding: undefined,
        parsedMeta: null,
        toolOutput: {
          kind: "outcome",
          text: "",
          isError: false,
          childConversationId: 1,
          durationMs: 1000,
        },
        state: "output-available",
        errorText: null,
        childAwaitingPermission: false,
        childTaskStatus: "running",
      })
    ).toBe("ok")
  })
})

describe("parseInput — historical agent types", () => {
  it("resolves agent_type grok for history reload without a live binding", () => {
    const parsed = parseInput(
      JSON.stringify({
        agent_type: "grok",
        task: "fix review findings",
        working_dir: "D:\\MyCodeBuddy",
      })
    )
    expect(parsed.agentType).toBe("grok")
    expect(ALL_AGENT_TYPES).toContain("grok")
    expect(AGENT_LABELS.grok).toBe("Grok")
    expect(parsed.task).toBe("fix review findings")
  })

  it("accepts every agent type in ALL_AGENT_TYPES", () => {
    for (const agentType of ALL_AGENT_TYPES) {
      const parsed = parseInput(
        JSON.stringify({ agent_type: agentType, task: "t" })
      )
      expect(parsed.agentType).toBe(agentType)
    }
  })
})

describe("formatDelegationDisplaySecondary", () => {
  it("prefers a non-empty formatted title over task", () => {
    expect(
      formatDelegationDisplaySecondary("Fix the login bug", "raw task text")
    ).toBe("Fix the login bug")
  })

  it("trims title whitespace before use", () => {
    expect(
      formatDelegationDisplaySecondary("  padded title  ", "task fallback")
    ).toBe("padded title")
  })

  it("falls through whitespace-only title to task", () => {
    expect(formatDelegationDisplaySecondary("   \t  ", "use the task")).toBe(
      "use the task"
    )
  })

  it("folds reference-link titles via formatConversationTitle", () => {
    expect(
      formatDelegationDisplaySecondary(
        "[README.md](file:///Users/x/README.md)",
        "ignored task"
      )
    ).toBe("README.md")
  })

  it("uses task when title is null/undefined/empty", () => {
    expect(formatDelegationDisplaySecondary(null, "only task")).toBe(
      "only task"
    )
    expect(formatDelegationDisplaySecondary(undefined, "only task")).toBe(
      "only task"
    )
    expect(formatDelegationDisplaySecondary("", "only task")).toBe("only task")
  })

  it("returns null when title and task are both empty", () => {
    expect(formatDelegationDisplaySecondary(null, null)).toBeNull()
    expect(formatDelegationDisplaySecondary(undefined, undefined)).toBeNull()
    expect(formatDelegationDisplaySecondary("", "")).toBeNull()
    expect(formatDelegationDisplaySecondary("  ", null)).toBeNull()
  })
})

describe("computeDelegationElapsedMs", () => {
  const startedAt = "2026-07-19T10:00:00.000Z"
  const finishedAt = "2026-07-19T10:00:05.000Z"
  const startedMs = Date.parse(startedAt)
  const finishedMs = Date.parse(finishedAt)

  it("running uses now - started when started is valid", () => {
    expect(
      computeDelegationElapsedMs({
        lifecycleStatus: "running",
        startedAt,
        finishedAt: null,
        completedDurationMs: null,
        nowMs: startedMs + 2500,
      })
    ).toBe(2500)
  })

  it("running ignores finishedAt and completedDurationMs", () => {
    expect(
      computeDelegationElapsedMs({
        lifecycleStatus: "running",
        startedAt,
        finishedAt,
        completedDurationMs: 99999,
        nowMs: startedMs + 1000,
      })
    ).toBe(1000)
  })

  it("terminal prefers finished - started when both valid", () => {
    expect(
      computeDelegationElapsedMs({
        lifecycleStatus: "ok",
        startedAt,
        finishedAt,
        completedDurationMs: 99999,
        nowMs: finishedMs + 10_000,
      })
    ).toBe(finishedMs - startedMs)
    expect(
      computeDelegationElapsedMs({
        lifecycleStatus: "err",
        startedAt,
        finishedAt,
        completedDurationMs: null,
        nowMs: finishedMs + 10_000,
      })
    ).toBe(finishedMs - startedMs)
  })

  it("terminal falls back to completedDurationMs when timestamps incomplete", () => {
    expect(
      computeDelegationElapsedMs({
        lifecycleStatus: "ok",
        startedAt,
        finishedAt: null,
        completedDurationMs: 1234,
        nowMs: startedMs + 99_000,
      })
    ).toBe(1234)
    expect(
      computeDelegationElapsedMs({
        lifecycleStatus: "ok",
        startedAt: null,
        finishedAt,
        completedDurationMs: 42,
        nowMs: finishedMs,
      })
    ).toBe(42)
  })

  it("returns null for invalid timestamps without duration fallback", () => {
    expect(
      computeDelegationElapsedMs({
        lifecycleStatus: "running",
        startedAt: "not-a-date",
        finishedAt: null,
        completedDurationMs: null,
        nowMs: Date.now(),
      })
    ).toBeNull()
    expect(
      computeDelegationElapsedMs({
        lifecycleStatus: "ok",
        startedAt: "bad",
        finishedAt: "also-bad",
        completedDurationMs: null,
        nowMs: Date.now(),
      })
    ).toBeNull()
  })

  it("returns null for negative elapsed or negative duration", () => {
    expect(
      computeDelegationElapsedMs({
        lifecycleStatus: "running",
        startedAt,
        finishedAt: null,
        completedDurationMs: null,
        nowMs: startedMs - 1,
      })
    ).toBeNull()
    expect(
      computeDelegationElapsedMs({
        lifecycleStatus: "ok",
        startedAt: finishedAt,
        finishedAt: startedAt,
        completedDurationMs: null,
        nowMs: finishedMs,
      })
    ).toBeNull()
    expect(
      computeDelegationElapsedMs({
        lifecycleStatus: "ok",
        startedAt: null,
        finishedAt: null,
        completedDurationMs: -5,
        nowMs: Date.now(),
      })
    ).toBeNull()
  })

  it("treats zero elapsed and zero duration as valid", () => {
    expect(
      computeDelegationElapsedMs({
        lifecycleStatus: "running",
        startedAt,
        finishedAt: null,
        completedDurationMs: null,
        nowMs: startedMs,
      })
    ).toBe(0)
    expect(
      computeDelegationElapsedMs({
        lifecycleStatus: "ok",
        startedAt: null,
        finishedAt: null,
        completedDurationMs: 0,
        nowMs: Date.now(),
      })
    ).toBe(0)
  })
})

describe("buildEditRollupViewModel", () => {
  function stats(
    overrides: Partial<DelegationRuntimeStats> = {}
  ): DelegationRuntimeStats {
    return {
      ...emptyRuntimeStats("2026-07-19T10:00:00.000Z"),
      ...overrides,
    }
  }

  it("returns files mode when touched_files is non-empty", () => {
    expect(
      buildEditRollupViewModel(
        stats({
          touched_files: [
            { path: "a.ts", outside_workspace: false },
            { path: "b.ts", outside_workspace: false },
          ],
          touched_files_truncated: false,
        })
      )
    ).toEqual({
      mode: "files",
      fileCount: 2,
      fileCountTruncated: false,
      additions: null,
      deletions: null,
      showLineTotals: false,
    })
  })

  it("marks fileCountTruncated when touched_files_truncated is true", () => {
    expect(
      buildEditRollupViewModel(
        stats({
          touched_files: [{ path: "a.ts", outside_workspace: false }],
          touched_files_truncated: true,
        })
      )
    ).toMatchObject({
      mode: "files",
      fileCount: 1,
      fileCountTruncated: true,
    })
  })

  it("shows line totals only when complete and both sides non-null", () => {
    expect(
      buildEditRollupViewModel(
        stats({
          touched_files: [{ path: "a.ts", outside_workspace: false }],
          line_counts_complete: true,
          additions: 4,
          deletions: 1,
        })
      )
    ).toEqual({
      mode: "files",
      fileCount: 1,
      fileCountTruncated: false,
      additions: 4,
      deletions: 1,
      showLineTotals: true,
    })
  })

  it("hides line totals when counts are partial or incomplete", () => {
    expect(
      buildEditRollupViewModel(
        stats({
          touched_files: [{ path: "a.ts", outside_workspace: false }],
          line_counts_complete: true,
          additions: 4,
          deletions: null,
        })
      )
    ).toMatchObject({ mode: "files", showLineTotals: false })
    expect(
      buildEditRollupViewModel(
        stats({
          touched_files: [{ path: "a.ts", outside_workspace: false }],
          line_counts_complete: false,
          additions: 4,
          deletions: 1,
        })
      )
    ).toMatchObject({ mode: "files", showLineTotals: false })
  })

  it("falls back to editCalls when paths empty but edit_tool_call_count > 0", () => {
    expect(
      buildEditRollupViewModel(
        stats({
          touched_files: [],
          edit_tool_call_count: 3,
        })
      )
    ).toEqual({ mode: "editCalls", editCallCount: 3 })
  })

  it("omits when no paths and no edit calls", () => {
    expect(
      buildEditRollupViewModel(
        stats({
          touched_files: [],
          edit_tool_call_count: 0,
        })
      )
    ).toEqual({ mode: "omit" })
  })

  it("omits when stats is null", () => {
    expect(buildEditRollupViewModel(null)).toEqual({ mode: "omit" })
  })
})

describe("parseToolOutput — durationMs retention", () => {
  it("retains non-negative duration_ms on completed reports", () => {
    const parsed = parseToolOutput(
      JSON.stringify({
        status: "completed",
        text: "done",
        child_conversation_id: 7,
        duration_ms: 1500,
      })
    )
    expect(parsed).toEqual({
      kind: "outcome",
      text: "done",
      isError: false,
      childConversationId: 7,
      durationMs: 1500,
    })
  })

  it("accepts zero duration_ms", () => {
    const parsed = parseToolOutput(
      JSON.stringify({
        status: "completed",
        text: "instant",
        duration_ms: 0,
      })
    )
    expect(parsed).toMatchObject({
      kind: "outcome",
      durationMs: 0,
    })
  })

  it("drops negative or non-finite duration_ms", () => {
    expect(
      parseToolOutput(
        JSON.stringify({
          status: "completed",
          text: "x",
          duration_ms: -1,
        })
      )
    ).toMatchObject({ kind: "outcome", durationMs: null })
    expect(
      parseToolOutput(
        JSON.stringify({
          status: "completed",
          text: "x",
          duration_ms: Number.NaN,
        })
      )
    ).toMatchObject({ kind: "outcome", durationMs: null })
  })

  it("sets durationMs null when duration_ms is absent", () => {
    expect(
      parseToolOutput(
        JSON.stringify({
          status: "completed",
          text: "no duration",
        })
      )
    ).toMatchObject({ kind: "outcome", durationMs: null })
  })

  it("preserves duration_ms through MCP structuredContent envelopes", () => {
    const parsed = parseToolOutput(
      JSON.stringify({
        content: [{ type: "text", text: "ok" }],
        structuredContent: {
          status: "completed",
          text: "from structured",
          duration_ms: 42,
          child_conversation_id: 3,
        },
      })
    )
    expect(parsed).toEqual({
      kind: "outcome",
      text: "from structured",
      isError: false,
      childConversationId: 3,
      durationMs: 42,
    })
  })

  it("does not attach durationMs to running acks", () => {
    const parsed = parseToolOutput(
      JSON.stringify({
        status: "running",
        child_conversation_id: 1,
        duration_ms: 99,
      })
    )
    expect(parsed).toEqual({
      kind: "ack",
      childConversationId: 1,
    })
  })
})
