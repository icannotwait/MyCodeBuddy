import { describe, expect, it } from "vitest"

import {
  parseDelegationMeta,
  parseInput,
  resolveDelegationStatus,
} from "@/lib/delegation-card"
import type { DelegationBinding } from "@/contexts/delegation-context"
import {
  AGENT_LABELS,
  ALL_AGENT_TYPES,
  emptyRuntimeStats,
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
