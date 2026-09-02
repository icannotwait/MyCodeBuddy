import { describe, expect, it } from "vitest"

import {
  buildEditRollupViewModel,
  computeDelegationElapsedMs,
  formatDelegationDisplaySecondary,
  isAffirmedResume,
  isRefusedResume,
  parseDelegateRunIdentity,
  parseDelegateTaskId,
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
    task: null,
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
      agentType: null,
      task: null,
      taskId: "task-abc",
      childConnectionId: "child-conn",
      childConversationId: 42,
      errorCode: null,
      startedAt: "2026-07-19T10:00:00.000Z",
      finishedAt: "2026-07-19T10:05:00.000Z",
      runtimeStats,
      attentionRequest,
      textPreview: "done preview",
      generation: null,
      syntheticHistorical: false,
    })
  })

  it("parses exact historical run correlation fields", () => {
    expect(
      parseDelegationMeta({
        "codeg.delegation": {
          status: "completed",
          agent_type: "codex",
          task_id: "run-3",
          child_conversation_id: 42,
          generation: 3,
          synthetic_historical: true,
        },
      })
    ).toMatchObject({
      agentType: "codex",
      taskId: "run-3",
      childConversationId: 42,
      generation: 3,
      syntheticHistorical: true,
    })
  })

  it.each([42, "unknown-agent"])(
    "rejects invalid historical agent_type %j",
    (agentType) => {
      expect(
        parseDelegationMeta({
          "codeg.delegation": {
            status: "completed",
            agent_type: agentType,
          },
        })?.agentType
      ).toBeNull()
    }
  )

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
          errorCode: null,
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
      agentType: null,
      errorCode: null,
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
      errorCode: null,
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
    ).toMatchObject({ kind: "outcome", durationMs: null, errorCode: null })
    expect(
      parseToolOutput(
        JSON.stringify({
          status: "completed",
          text: "x",
          duration_ms: Number.NaN,
        })
      )
    ).toMatchObject({ kind: "outcome", durationMs: null, errorCode: null })
  })

  it("sets durationMs null when duration_ms is absent", () => {
    expect(
      parseToolOutput(
        JSON.stringify({
          status: "completed",
          text: "no duration",
        })
      )
    ).toMatchObject({ kind: "outcome", durationMs: null, errorCode: null })
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
      agentType: null,
      errorCode: null,
    })
  })

  it("ignores wire duration_ms on running acks", () => {
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
      durationMs: null,
      agentType: null,
      errorCode: null,
    })
  })
})

describe("parseToolOutput — correlation / provisional error codes", () => {
  it("extracts error_code from failed task reports", () => {
    const parsed = parseToolOutput(
      JSON.stringify({
        status: "failed",
        error_code: "delegation_correlation_missing",
        message:
          "Parent tool call could not be correlated. Not unresumable. Do not replace.",
      })
    )
    expect(parsed).toEqual({
      kind: "outcome",
      text: "Parent tool call could not be correlated. Not unresumable. Do not replace.",
      isError: true,
      childConversationId: null,
      durationMs: null,
      agentType: null,
      errorCode: "delegation_correlation_missing",
    })
  })

  it("extracts error_code from MCP structuredContent correlation failures", () => {
    const parsed = parseToolOutput(
      JSON.stringify({
        content: [
          {
            type: "text",
            text: "correlation timed out; not unresumable",
          },
        ],
        isError: true,
        structuredContent: {
          status: "failed",
          error_code: "delegation_correlation_timeout",
          message: "correlation timed out; not unresumable",
        },
      })
    )
    expect(parsed).toMatchObject({
      kind: "outcome",
      isError: true,
      errorCode: "delegation_correlation_timeout",
    })
  })

  it("extracts legacy kind:err code field", () => {
    const parsed = parseToolOutput(
      JSON.stringify({
        kind: "err",
        code: "delegation_correlation_conflict",
        message: "conflict tombstone; not unresumable",
      })
    )
    expect(parsed).toMatchObject({
      kind: "outcome",
      isError: true,
      errorCode: "delegation_correlation_conflict",
    })
  })

  it("never promotes correlation_id into errorCode", () => {
    const parsed = parseToolOutput(
      JSON.stringify({
        status: "failed",
        error_code: "delegation_correlation_ambiguous",
        correlation_id: "should-not-become-error-code",
        message: "ambiguous match",
      })
    )
    expect(parsed).toMatchObject({
      kind: "outcome",
      errorCode: "delegation_correlation_ambiguous",
    })
    if (parsed?.kind === "outcome") {
      expect(parsed.errorCode).not.toBe("should-not-become-error-code")
      expect(parsed.text).not.toContain("should-not-become-error-code")
    }
  })
})

describe("parseInput — correlation_id is transport-only", () => {
  it("extracts task/agent without surfacing correlation_id", () => {
    const parsed = parseInput(
      JSON.stringify({
        agent_type: "codex",
        task: "ship the fix",
        correlation_id: "corr-never-display-me",
      })
    )
    expect(parsed).toEqual({
      agentType: "codex",
      profileLabel: null,
      task: "ship the fix",
      workingDir: null,
      workUnitKey: null,
      targetTaskId: null,
      replacesTaskId: null,
    })
    expect(JSON.stringify(parsed)).not.toContain("corr-never-display-me")
  })
})

describe("parseInput wrapper peeling", () => {
  it("peels Cursor's MCP args wrapper", () => {
    const parsed = parseInput(
      JSON.stringify({
        providerIdentifier: "codeg-mcp",
        toolName: "delegate_to_agent",
        args: { agent_type: "claude_code", task: "run pnpm build" },
      })
    )
    expect(parsed.agentType).toBe("claude_code")
    expect(parsed.task).toBe("run pnpm build")
    expect(parsed.workingDir).toBeNull()
  })

  it("reads structured work-unit and continuation identity fields", () => {
    expect(
      parseInput(
        JSON.stringify({
          task: "continue",
          task_id: "run-1",
          work_unit_key: "task|1|implementer|grok|none",
          replaces_task_id: "run-0",
        })
      )
    ).toMatchObject({
      targetTaskId: "run-1",
      workUnitKey: "task|1|implementer|grok|none",
      replacesTaskId: "run-0",
    })
  })

  it("does not infer identity fields from arbitrary task text", () => {
    expect(
      parseInput(
        JSON.stringify({
          task: "use work_unit_key=fake and task_id=also-fake",
        })
      )
    ).toMatchObject({
      targetTaskId: null,
      workUnitKey: null,
      replacesTaskId: null,
    })
  })

  it("returns empty fields for a non-delegation payload", () => {
    const parsed = parseInput(JSON.stringify({ command: "ls -la" }))
    expect(parsed.agentType).toBeNull()
    expect(parsed.task).toBeNull()
  })

  // Guards the allowlist against drifting behind the canonical agent list — the
  // regression that left `grok` and `cursor` delegation cards iconless. Every
  // known agent must resolve so its sub-agent card shows the right icon/label.
  it.each(ALL_AGENT_TYPES)("recognizes the %s agent_type", (agentType) => {
    const parsed = parseInput(
      JSON.stringify({ agent_type: agentType, task: "do the thing" })
    )
    expect(parsed.agentType).toBe(agentType)
  })
})

describe("parseDelegateRunIdentity", () => {
  it("links a continued output run to its structured target task", () => {
    expect(
      parseDelegateRunIdentity({
        parentConversationId: 2075,
        parentToolUseId: "tool-2",
        input: JSON.stringify({ task: "continue", task_id: "run-1" }),
        output: JSON.stringify({
          structuredContent: {
            task_id: "run-2",
            continued_from_task_id: "run-1",
            child_conversation_id: 3001,
            status: "running",
          },
        }),
        errorText: null,
        meta: null,
      })
    ).toMatchObject({
      parentConversationId: 2075,
      parentToolUseId: "tool-2",
      taskId: "run-2",
      childConversationId: 3001,
      linkedTaskIds: ["run-1"],
    })
  })

  it("uses metadata as a durable identity fallback", () => {
    expect(
      parseDelegateRunIdentity({
        parentConversationId: 2075,
        parentToolUseId: "tool-history",
        input: null,
        output: null,
        errorText: null,
        meta: {
          "codeg.delegation": {
            status: "completed",
            task_id: "run-history",
            child_conversation_id: 3002,
          },
        },
      })
    ).toMatchObject({
      taskId: "run-history",
      childConversationId: 3002,
      linkedTaskIds: [],
    })
  })

  it("keeps the child id on a synthetic historical failure that has no current run id", () => {
    expect(
      parseDelegateRunIdentity({
        parentConversationId: 3866,
        parentToolUseId: "child-3880",
        input: JSON.stringify({ agent_type: "codex", task: "canceled" }),
        output: JSON.stringify({
          status: "canceled",
          child_conversation_id: 3880,
          error_code: "usercancel",
          message: "usercancel",
        }),
        errorText: "usercancel",
        meta: {
          "codeg.delegation": {
            status: "err",
            child_conversation_id: 3880,
            error_code: "usercancel",
            synthetic_historical: true,
          },
        },
      })
    ).toMatchObject({
      taskId: null,
      childConversationId: 3880,
    })
  })

  it("does not group an uncorrelated failed run by an echoed child id", () => {
    expect(
      parseDelegateRunIdentity({
        parentConversationId: 2075,
        parentToolUseId: "tool-failed",
        input: JSON.stringify({ task: "continue" }),
        output: JSON.stringify({
          content: [{ type: "text", text: "Continuation admission failed" }],
          structuredContent: {
            status: "failed",
            child_conversation_id: 3001,
            error_code: "continuation_admission_failed",
            message: "Continuation admission failed",
          },
          isError: true,
        }),
        errorText: null,
        meta: null,
      })
    ).toMatchObject({
      taskId: null,
      childConversationId: null,
    })
  })
})

describe("codex live-wire result envelope", () => {
  /**
   * codex-acp forwards every MCP call's outcome as
   * `rawOutput = { result: <CallToolResult>, error: <string|null> }`
   * (`createMcpRawOutput`) — one layer above the shapes the parsers read.
   */
  function codexLive(callToolResult: Record<string, unknown>): string {
    return JSON.stringify({
      error: null,
      result: { meta: null, ...callToolResult },
    })
  }

  const runningAck = {
    agent_type: "codex",
    child_conversation_id: 2781,
    status: "running",
    task_id: "8cb72a7c-1a96-44aa-9c26-d4356862c9c2",
    message:
      "Delegation successful. task_id=8cb72a7c-1a96-44aa-9c26-d4356862c9c2.",
  }

  it("reads a running ack through the wrapper (ack, not a terminal outcome)", () => {
    const parsed = parseToolOutput(
      codexLive({
        content: [{ type: "text", text: runningAck.message }],
        structuredContent: runningAck,
      })
    )
    expect(parsed).toEqual({
      kind: "ack",
      childConversationId: 2781,
      durationMs: null,
      agentType: "codex",
      // No refusal code — this is a real ack. See `isRefusedResume`.
      errorCode: null,
    })
  })

  // Note: this one already passed pre-fix via the `task_id=<id>` text scan —
  // it guards that the structured path doesn't regress that resolution.
  it("resolves the task id through the wrapper", () => {
    const output = codexLive({
      content: [{ type: "text", text: "Delegation successful." }],
      structuredContent: runningAck,
    })
    expect(parseDelegateTaskId(output, null)).toBe(runningAck.task_id)
  })

  it("surfaces a failed envelope's error string instead of the raw JSON", () => {
    const parsed = parseToolOutput(
      JSON.stringify({ error: "mcp server disconnected", result: null })
    )
    expect(parsed).toEqual({
      kind: "outcome",
      text: "mcp server disconnected",
      isError: true,
      childConversationId: null,
      durationMs: null,
      errorCode: null,
    })
  })

  it("leaves a child's nested {status, task_id} payload alone", () => {
    // Peeling on the `result` KEY alone would turn opaque child output into a
    // failed delegation outcome and hand out `child-job` as the task id.
    const childOutput = JSON.stringify({
      result: { status: "failed", task_id: "child-job", message: "domain" },
    })
    expect(parseToolOutput(childOutput)).toEqual({
      kind: "outcome",
      text:
        "```json\n" +
        JSON.stringify(JSON.parse(childOutput), null, 2) +
        "\n```",
      isError: false,
      childConversationId: null,
      durationMs: null,
      errorCode: null,
    })
    expect(parseDelegateTaskId(childOutput, null)).toBeNull()
  })

  it("does NOT treat a child's own `error` field as a host failure", () => {
    // No `result` key ⇒ not codex-acp's failure envelope. Must stay a
    // non-error outcome rendered as-is.
    const childOutput = JSON.stringify({ error: "domain validation", rows: [] })
    const parsed = parseToolOutput(childOutput)
    expect(parsed).toMatchObject({ kind: "outcome", isError: false })
    expect(parsed).not.toMatchObject({ text: "domain validation" })
  })
})

describe("parseDelegationMeta task fields", () => {
  it("surfaces broker-stamped task_preview and task_id", () => {
    const parsed = parseDelegationMeta({
      "codeg.delegation": {
        status: "running",
        child_conversation_id: 42,
        task_preview: "run pnpm build",
        task_id: "task-uuid-1",
      },
    })
    expect(parsed?.task).toBe("run pnpm build")
    expect(parsed?.taskId).toBe("task-uuid-1")
    expect(parsed?.childConversationId).toBe(42)
  })

  it("keeps task fields null for older or malformed meta", () => {
    expect(
      parseDelegationMeta({
        "codeg.delegation": { status: "completed" },
      })
    ).toMatchObject({ task: null, taskId: null })
    expect(
      parseDelegationMeta({
        "codeg.delegation": {
          status: "running",
          task_preview: "",
          task_id: 7,
        },
      })
    ).toMatchObject({ task: null, taskId: null })
  })

  it("surfaces the agent_type the historical injection supplies", () => {
    // Written only by `build_historical_delegation_meta` (from the child's DB
    // row). It is the agent-type source for a reloaded `resume_delegation`
    // card, whose arguments are just `{task_id, reason}`.
    const parsed = parseDelegationMeta({
      "codeg.delegation": { status: "completed", agent_type: "codex" },
    })
    expect(parsed?.agentType).toBe("codex")
  })

  it("rejects an unrecognized agent_type but keeps a custom one", () => {
    expect(
      parseDelegationMeta({
        "codeg.delegation": { status: "running", agent_type: "not_an_agent" },
      })?.agentType
    ).toBeNull()
    expect(
      parseDelegationMeta({
        "codeg.delegation": { status: "running", agent_type: "custom:my-cli" },
      })?.agentType
    ).toBe("custom:my-cli")
  })
})

describe("parseToolOutput agent type", () => {
  it("reads agent_type off the broker report", () => {
    // `delegate_to_agent` merely echoes its own argument here, but a
    // `resume_delegation` result is the ONLY place a reloaded card can learn
    // which agent came back.
    expect(
      parseToolOutput(
        JSON.stringify({
          task_id: "t-1",
          status: "running",
          agent_type: "codex",
          child_conversation_id: 9,
        })
      )
    ).toMatchObject({ kind: "ack", agentType: "codex", childConversationId: 9 })
  })

  it("carries agent_type onto a terminal outcome too", () => {
    expect(
      parseToolOutput(
        JSON.stringify({
          task_id: "t-1",
          status: "completed",
          agent_type: "claude_code",
          text: "all green",
        })
      )
    ).toMatchObject({
      kind: "outcome",
      agentType: "claude_code",
      isError: false,
    })
  })

  it("leaves agentType null when the report omits it", () => {
    expect(
      parseToolOutput(JSON.stringify({ task_id: "t-1", status: "running" }))
    ).toMatchObject({ kind: "ack", agentType: null })
  })
})

describe("isRefusedResume", () => {
  // `not_resumable_report` (broker.rs) reports the task's REAL status, so a
  // refusal and a genuine resume differ only by `error_code`. Reading status
  // alone paints "Not resumed: it already completed" as a finished sub-agent.
  it.each(["completed", "running", "canceled"])(
    "recognizes a refusal reported with status %s",
    (status) => {
      expect(
        isRefusedResume(
          JSON.stringify({
            task_id: "t-1",
            status,
            error_code: "not_resumable",
            agent_type: "codex",
            child_conversation_id: 9,
            message: "Not resumed: the task already completed.",
          })
        )
      ).toBe(true)
    }
  )

  it("leaves a genuine resume ack alone", () => {
    expect(
      isRefusedResume(
        JSON.stringify({
          task_id: "t-1",
          status: "running",
          agent_type: "codex",
          child_conversation_id: 9,
          message: "Delegation resumed.",
        })
      )
    ).toBe(false)
  })

  // An unknown task id is refused too, but by a report that names no agent and
  // no child — nothing for a card to draw, so `hasModel` already handles it.
  it("does not claim an unknown-task report", () => {
    expect(
      isRefusedResume(
        JSON.stringify({
          task_id: "t-1",
          status: "unknown",
          message: "Unknown task id",
        })
      )
    ).toBe(false)
    expect(isRefusedResume(null)).toBe(false)
  })

  it("reads a refusal delivered as an error", () => {
    expect(
      isRefusedResume(
        null,
        JSON.stringify({
          task_id: "t-1",
          status: "failed",
          error_code: "not_resumable",
        })
      )
    ).toBe(true)
  })
})

// `render_task_report` keeps the whole report in `structuredContent` and
// renders only `message` as content text. OpenCode drops `structuredContent`
// wholesale ("the human-readable lines ARE the whole record",
// `acp/connection.rs`), so on those hosts the prefix is the only signal left.
describe("resume verdicts on hosts that drop structuredContent", () => {
  const REFUSAL_TEXT =
    "Not resumed: the task already completed — resume only applies to a canceled task."
  const ACK_TEXT =
    "Delegation resumed. task_id=t-1 (unchanged). Call get_delegation_status with this id."
  const UNKNOWN_TEXT =
    "Unknown task id — it never existed, isn't owned by this session, or its result was evicted."

  // Bare text is what actually arrives: `opencode_live_tool_output` returns
  // None whenever `content` carries the result (letting the clean text render)
  // and otherwise unwraps `rawOutput.output` to the bare string, and the
  // history parser mirrors it. So the `{output: …}` envelope never reaches
  // these predicates — which is why the text check can stay anchored.
  it("still recognizes a refusal from the message text alone", () => {
    expect(isRefusedResume(REFUSAL_TEXT)).toBe(true)
  })

  it("affirms a resume from the ack text alone", () => {
    expect(isAffirmedResume(ACK_TEXT)).toBe(true)
  })

  // A foreign task id lands on `unknown_report`. It must NOT affirm, or the
  // card would adopt another conversation's binding by task id alone.
  it("does not affirm an unknown-task report or a refusal", () => {
    expect(isAffirmedResume(UNKNOWN_TEXT)).toBe(false)
    expect(isAffirmedResume(REFUSAL_TEXT)).toBe(false)
    expect(isAffirmedResume(null)).toBe(false)
  })

  it("keeps the structured verdict authoritative when it survived", () => {
    const structured = JSON.stringify({
      task_id: "t-1",
      status: "running",
      agent_type: "codex",
      child_conversation_id: 9,
      message: ACK_TEXT,
    })
    expect(isRefusedResume(structured)).toBe(false)
    expect(isAffirmedResume(structured)).toBe(true)
  })
})

// `render_task_report` renders `text` in preference to `message` for a
// `completed` report, and a resume whose child finished during setup
// (`broker.rs`'s `Disposition::ChildTerminal`) reports the child's OWN
// LLM-written output there. A sub-agent that merely talks about delegation
// must not be read as a verdict about its own card — which is why the text
// check is anchored rather than a substring scan.
describe("resume verdicts never read a child's prose as a verdict", () => {
  const CHILD_PROSE =
    "I reviewed the resume path. The broker answers `Not resumed: <why>` when it " +
    "refuses, and `Delegation resumed. task_id=…` when it succeeds."

  it("ignores both markers when they appear inside a completed child's text", () => {
    const structured = JSON.stringify({
      task_id: "t-1",
      status: "completed",
      agent_type: "codex",
      child_conversation_id: 9,
      text: CHILD_PROSE,
    })
    expect(isRefusedResume(structured)).toBe(false)
    // Structure survived, so the child id — not the prose — is the confirmation.
    expect(isAffirmedResume(structured)).toBe(true)
  })

  it("ignores them in bare child text on a structure-dropping host", () => {
    expect(isRefusedResume(CHILD_PROSE)).toBe(false)
    expect(isAffirmedResume(CHILD_PROSE)).toBe(false)
  })

  // A structured report that named no child is `unknown_report` — the foreign
  // task id case. It must not affirm even though nothing refused it either.
  it("does not affirm a structured report that named no child", () => {
    const unknown = JSON.stringify({
      task_id: "t-1",
      status: "unknown",
      message: "Unknown task id — it never existed.",
    })
    expect(isAffirmedResume(unknown)).toBe(false)
  })

  // The legacy synchronous `{kind: "ok"|"err"}` shape is a RECOGNIZED report
  // too, so it must never fall through to the text path — otherwise a
  // legitimate result whose text merely opens with a marker gets read as a
  // verdict about the call.
  it("treats the legacy kind-shaped outcome as structured", () => {
    const legacyOk = JSON.stringify({
      kind: "ok",
      child_conversation_id: 9,
      text: "Not resumed: is the phrase the broker uses when it declines.",
    })
    expect(isRefusedResume(legacyOk)).toBe(false)
    expect(isAffirmedResume(legacyOk)).toBe(true)

    const legacyErr = JSON.stringify({
      kind: "err",
      code: "spawn_failed",
      message: "Delegation resumed is the phrase used on success.",
    })
    expect(isRefusedResume(legacyErr)).toBe(false)
    // No child named ⇒ nothing corroborates a task-id binding.
    expect(isAffirmedResume(legacyErr)).toBe(false)
  })
})
