import { describe, expect, it } from "vitest"

import {
  parseDelegateRunIdentity,
  parseDelegationMeta,
  parseInput,
  parseToolOutput,
} from "@/lib/delegation-card"
import type { DelegationCardSource } from "@/hooks/use-delegation-card-model"
import type { DbConversationSummary } from "@/lib/types"
import {
  childConversationToDelegationSource,
  mergeDelegationSourceLayers,
  sortChildConversationsForOverlay,
} from "@/lib/delegation-overlay-history"

function child(
  overrides: Partial<DbConversationSummary> & Pick<DbConversationSummary, "id">
): DbConversationSummary {
  return {
    folder_id: 1,
    title: `child-${overrides.id}`,
    title_locked: false,
    auto_title_finalized: false,
    agent_type: "codex",
    status: "pending_review",
    awaiting_reply_token: null,
    kind: "delegate",
    model: null,
    git_branch: null,
    external_id: null,
    message_count: 0,
    child_count: 0,
    created_at: "2026-08-19T11:30:08.000Z",
    updated_at: "2026-08-19T11:41:46.000Z",
    pinned_at: null,
    parent_id: 3866,
    parent_tool_use_id: `exec-${overrides.id}`,
    delegation_call_id: `task-${overrides.id}`,
    delegation_task_status: "completed",
    ...overrides,
  }
}

function source(
  overrides: Partial<DelegationCardSource> &
    Pick<DelegationCardSource, "parentToolUseId">
): DelegationCardSource {
  return {
    parentConversationId: 3866,
    ...overrides,
  }
}

describe("childConversationToDelegationSource", () => {
  it("keeps the durable exec parent_tool_use_id without treating the root call id as the current run", () => {
    const source = childConversationToDelegationSource(
      3866,
      child({
        id: 3867,
        parent_tool_use_id: "exec-d7fc0870-0b42-421e-b466-e85a4a2d56f4",
        delegation_call_id: "b496089a-4eba-4643-9e58-74e76d35ec02",
        title: "Sale/ATM P1 review",
      })
    )

    expect(source.parentToolUseId).toBe(
      "exec-d7fc0870-0b42-421e-b466-e85a4a2d56f4"
    )
    expect(source.parentConversationId).toBe(3866)
    expect(parseInput(source.input).agentType).toBe("codex")
    expect(parseInput(source.input).task).toBe("Sale/ATM P1 review")
    expect(parseDelegateRunIdentity(source).taskId).toBeNull()
    expect(parseDelegateRunIdentity(source).childConversationId).toBe(3867)
    expect(parseToolOutput(source.output)?.kind).toBe("outcome")
    expect(parseDelegationMeta(source.meta)?.status).toBe("ok")
    expect(parseDelegationMeta(source.meta)?.taskId).toBeNull()
  })

  it("synthesizes a unique tool id when parent_tool_use_id is blank", () => {
    const source = childConversationToDelegationSource(
      3866,
      child({ id: 3871, parent_tool_use_id: "   " })
    )
    expect(source.parentToolUseId).toBe("child-3871")
  })

  it("keeps a running child as an ack instead of a completed outcome", () => {
    const source = childConversationToDelegationSource(
      3866,
      child({
        id: 3879,
        delegation_task_status: "running",
        status: "in_progress",
      })
    )
    expect(source.state).toBe("input-available")
    expect(parseToolOutput(source.output)).toEqual({
      kind: "ack",
      childConversationId: 3879,
      durationMs: null,
      agentType: null,
      errorCode: null,
    })
    expect(parseDelegationMeta(source.meta)?.status).toBe("running")
  })

  it("falls back to conversation status when delegation_task_status is missing", () => {
    const source = childConversationToDelegationSource(
      3866,
      child({
        id: 3881,
        delegation_task_status: null,
        status: "in_progress",
      })
    )
    expect(source.state).toBe("input-available")
    expect(parseToolOutput(source.output)).toEqual({
      kind: "ack",
      childConversationId: 3881,
      durationMs: null,
      agentType: null,
      errorCode: null,
    })
    expect(parseDelegationMeta(source.meta)?.status).toBe("running")
  })

  it("maps canceled children to a failed card without dropping the child id", () => {
    const source = childConversationToDelegationSource(
      3866,
      child({
        id: 3880,
        delegation_task_status: "canceled",
        delegation_error_code: "usercancel",
      })
    )
    expect(source.state).toBe("output-error")
    expect(parseToolOutput(source.output, true)?.isError).toBe(true)
    expect(parseDelegateRunIdentity(source).childConversationId).toBe(3880)
    expect(parseDelegationMeta(source.meta)?.status).toBe("err")
  })
})

describe("sortChildConversationsForOverlay", () => {
  it("orders oldest launches first even when the API is newest-first", () => {
    const ordered = sortChildConversationsForOverlay([
      child({
        id: 3871,
        created_at: "2026-08-19T11:30:23.000Z",
        delegation_started_at: "2026-08-19T11:30:23.000Z",
      }),
      child({
        id: 3867,
        created_at: "2026-08-19T11:30:08.000Z",
        delegation_started_at: "2026-08-19T11:30:08.000Z",
      }),
    ])
    expect(ordered.map((row) => row.id)).toEqual([3867, 3871])
  })
})

describe("mergeDelegationSourceLayers", () => {
  it("fills an empty transcript from durable children after compaction", () => {
    const durable = [
      childConversationToDelegationSource(3866, child({ id: 3867 })),
      childConversationToDelegationSource(3866, child({ id: 3868 })),
    ]
    expect(mergeDelegationSourceLayers(durable, [])).toEqual(durable)
  })

  it("lets a later continue_delegation replace the same child without duplicating it", () => {
    const durable = childConversationToDelegationSource(
      3866,
      child({
        id: 3869,
        parent_tool_use_id: "exec-first",
        delegation_call_id: "task-first",
      })
    )
    const continued = source({
      parentToolUseId: "exec-continue",
      parentConversationId: 3866,
      input: JSON.stringify({
        agent_type: "codex",
        task_id: "task-first",
        task: "please emit the report",
      }),
      output: JSON.stringify({
        status: "completed",
        task_id: "task-second",
        child_conversation_id: 3869,
      }),
    })

    const merged = mergeDelegationSourceLayers([durable], [continued])
    expect(merged).toHaveLength(1)
    expect(merged[0].parentToolUseId).toBe("exec-continue")
    expect(parseDelegateRunIdentity(merged[0]).childConversationId).toBe(3869)
  })

  it("correlates a transcript root task id with the durable child without exposing that id as the current run", () => {
    const durable = childConversationToDelegationSource(
      3866,
      child({
        id: 3867,
        parent_tool_use_id: "exec-root",
        delegation_call_id: "root-task",
      })
    )
    const inflight = source({
      parentToolUseId: "pt-live",
      parentConversationId: 3866,
      input: JSON.stringify({ agent_type: "codex", task: "just launched" }),
      output: JSON.stringify({ status: "running", task_id: "root-task" }),
    })

    const merged = mergeDelegationSourceLayers([durable], [inflight])
    expect(merged).toHaveLength(1)
    expect(merged[0].parentToolUseId).toBe("pt-live")
    expect(parseDelegateRunIdentity(durable).taskId).toBeNull()
  })

  it("lets a live continuation replace the latest historical row of the same child", () => {
    const first = source({
      parentToolUseId: "exec-first",
      input: JSON.stringify({ agent_type: "codex", task: "first" }),
      output: JSON.stringify({
        status: "completed",
        task_id: "task-first",
        child_conversation_id: 3869,
      }),
    })
    const second = source({
      parentToolUseId: "exec-second",
      input: JSON.stringify({
        agent_type: "codex",
        task_id: "task-first",
        task: "second",
      }),
      output: JSON.stringify({
        status: "completed",
        task_id: "task-second",
        child_conversation_id: 3869,
      }),
    })
    const live = source({
      parentToolUseId: "exec-live",
      input: JSON.stringify({
        agent_type: "codex",
        task_id: "task-second",
        task: "third",
      }),
      output: JSON.stringify({
        status: "running",
        task_id: "task-third",
        child_conversation_id: 3869,
      }),
    })

    const merged = mergeDelegationSourceLayers([first, second], [live])
    expect(merged.map((row) => row.parentToolUseId)).toEqual([
      "exec-first",
      "exec-live",
    ])
  })

  it("keeps transcript-only in-flight rows that have no child yet", () => {
    const durable = childConversationToDelegationSource(
      3866,
      child({ id: 3867 })
    )
    const inflight = source({
      parentToolUseId: "pt-live",
      parentConversationId: 3866,
      input: JSON.stringify({ agent_type: "codex", task: "just launched" }),
      output: JSON.stringify({ status: "running", task_id: "task-live" }),
    })
    const merged = mergeDelegationSourceLayers([durable], [inflight])
    expect(merged.map((row) => row.parentToolUseId)).toEqual([
      `exec-3867`,
      "pt-live",
    ])
  })
})
