import { describe, expect, it } from "vitest"

import { buildDelegationSeedEnvelopes } from "@/lib/delegation-seed"
import { emptyRuntimeStats, type ActiveDelegationState } from "@/lib/types"

function dele(
  overrides: Partial<ActiveDelegationState> & { parent_tool_use_id: string }
): ActiveDelegationState {
  return {
    child_connection_id: "c1",
    child_conversation_id: 99,
    agent_type: "codex",
    task_id: "task-1",
    started_at: "2026-07-19T00:00:00.000Z",
    runtime_stats: emptyRuntimeStats(),
    ...overrides,
  }
}

describe("buildDelegationSeedEnvelopes", () => {
  it("seeds a delegation_started per running delegation, carrying the parent connection id", () => {
    const env = buildDelegationSeedEnvelopes(
      "parent-conn",
      [dele({ parent_tool_use_id: "pt-1" })],
      42
    )
    expect(env).toHaveLength(1)
    expect(env[0]).toMatchObject({
      seq: 42,
      connection_id: "parent-conn",
      type: "delegation_started",
      parent_connection_id: "parent-conn",
      parent_tool_use_id: "pt-1",
      child_connection_id: "c1",
      child_conversation_id: 99,
      agent_type: "codex",
    })
  })

  it("preserves order and emits one started envelope per delegation", () => {
    const env = buildDelegationSeedEnvelopes(
      "p",
      [
        dele({ parent_tool_use_id: "pt-a", child_conversation_id: 1 }),
        dele({ parent_tool_use_id: "pt-b", child_conversation_id: 2 }),
        dele({ parent_tool_use_id: "pt-c", child_conversation_id: 3 }),
      ],
      5
    )
    expect(env.map((e) => e.type)).toEqual([
      "delegation_started",
      "delegation_started",
      "delegation_started",
    ])
    expect(
      env.map((e) =>
        "parent_tool_use_id" in e ? e.parent_tool_use_id : undefined
      )
    ).toEqual(["pt-a", "pt-b", "pt-c"])
  })

  it("returns an empty array for no delegations", () => {
    expect(buildDelegationSeedEnvelopes("p", [], 0)).toEqual([])
  })

  it.each([
    ["active", null],
    ["waiting_input", null],
    ["stalled", "2026-07-17T12:00:00Z"],
  ] as const)(
    "seeds observation=%s (and activity times) from active_delegations",
    (observation, stalledSince) => {
      const env = buildDelegationSeedEnvelopes(
        "parent-conn",
        [
          dele({
            parent_tool_use_id: "pt-1",
            observation,
            last_agent_activity_at: "2026-07-17T11:59:00Z",
            stalled_since: stalledSince,
          }),
        ],
        7
      )
      expect(env).toHaveLength(1)
      expect(env[0]).toMatchObject({
        type: "delegation_started",
        observation,
        last_agent_activity_at: "2026-07-17T11:59:00Z",
        stalled_since: stalledSince,
      })
    }
  )

  it("defaults missing snapshot observation to active (not invented terminals)", () => {
    const env = buildDelegationSeedEnvelopes(
      "p",
      [dele({ parent_tool_use_id: "pt-x" })],
      1
    )
    expect(env[0]).toMatchObject({
      type: "delegation_started",
      observation: "active",
      last_agent_activity_at: null,
      stalled_since: null,
    })
  })

  it("forwards task_id, started_at, runtime_stats, attention_request", () => {
    const runtimeStats = {
      ...emptyRuntimeStats("2026-07-19T08:00:00.000Z"),
      tool_call_count: 2,
      edit_tool_call_count: 1,
      additions: 5,
      deletions: 0,
      line_counts_complete: true,
    }
    const attentionRequest = {
      request_id: "att-9",
      task_id: "task-seed",
      message: "Approve file write?",
      created_at: "2026-07-19T08:01:00.000Z",
    }
    const env = buildDelegationSeedEnvelopes(
      "parent-conn",
      [
        dele({
          parent_tool_use_id: "pt-seed",
          task_id: "task-seed",
          started_at: "2026-07-19T08:00:00.000Z",
          runtime_stats: runtimeStats,
          attention_request: attentionRequest,
        }),
      ],
      11
    )
    expect(env).toHaveLength(1)
    expect(env[0]).toMatchObject({
      type: "delegation_started",
      task_id: "task-seed",
      started_at: "2026-07-19T08:00:00.000Z",
      runtime_stats: runtimeStats,
      attention_request: attentionRequest,
    })
  })
})
