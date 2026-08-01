import { describe, expect, it, vi } from "vitest"

import type {
  AdaptedContentPart,
  AdaptedDelegationWorkUnitPart,
  AdaptedMessage,
  AdaptedToolCallPart,
} from "@/lib/adapters/ai-elements-adapter"
import { buildDelegationTaskRows } from "@/lib/delegation-status"
import {
  projectDelegationTranscript,
  shouldFoldLiveDelegationTool,
} from "@/lib/delegation-transcript-projection"

function assistant(
  id: string,
  ...content: AdaptedContentPart[]
): AdaptedMessage {
  return {
    id,
    role: "assistant",
    content,
    timestamp: `2026-07-27T00:00:${id.padStart(2, "0")}Z`,
  }
}

function delegate(
  toolCallId: string,
  taskId: string,
  workUnitKey: string | null,
  options: {
    toolName?: "delegate_to_agent" | "continue_delegation"
    targetTaskId?: string | null
    childConversationId?: number
  } = {}
): AdaptedToolCallPart {
  const targetTaskId = options.targetTaskId ?? null
  return {
    type: "tool-call",
    toolCallId,
    toolName: options.toolName ?? "delegate_to_agent",
    input: JSON.stringify({
      task: "implement",
      ...(workUnitKey ? { work_unit_key: workUnitKey } : {}),
      ...(targetTaskId ? { task_id: targetTaskId } : {}),
    }),
    output: JSON.stringify({
      content: [{ type: "text", text: `Delegated ${taskId}` }],
      structuredContent: {
        status: "running",
        task_id: taskId,
        child_conversation_id: options.childConversationId ?? 3001,
        ...(targetTaskId ? { continued_from_task_id: targetTaskId } : {}),
      },
    }),
    state: "output-available",
  }
}

type ReportStatus = "running" | "completed" | "failed" | "canceled"

function status(
  toolCallId: string,
  reports: Array<{ taskId: string; status: ReportStatus }>
): AdaptedContentPart {
  return {
    type: "delegation-status-group",
    polls: [
      {
        type: "tool-call",
        toolCallId,
        toolName: "get_delegation_status",
        input: JSON.stringify({
          task_ids: reports.map((report) => report.taskId),
        }),
        output: JSON.stringify({
          content: [{ type: "text", text: "status batch" }],
          structuredContent: {
            tasks: reports.map((report) => ({
              task_id: report.taskId,
              status: report.status,
              ...(report.status === "completed"
                ? { text: "done" }
                : { message: "Running." }),
            })),
          },
        }),
        state: "output-available",
      },
    ],
  }
}

const statusCases = [
  {
    name: "input-only timeout",
    input: { task_ids: ["run-1"] },
    reports: null,
    error: "timed out awaiting tools/call after 300s",
    fold: true,
  },
  {
    name: "output-only structured",
    input: null,
    reports: [{ task_id: "run-1", status: "running" }],
    error: null,
    fold: true,
  },
  {
    name: "agreed known",
    input: { task_ids: ["run-1"] },
    reports: [{ task_id: "run-1", status: "running" }],
    error: null,
    fold: true,
  },
  {
    name: "legacy known",
    input: { task_id: "run-1" },
    reports: null,
    error: null,
    fold: true,
  },
  {
    name: "mixed known unknown",
    input: { task_ids: ["run-1", "unknown"] },
    reports: [
      { task_id: "run-1", status: "running" },
      { task_id: "unknown", status: "running" },
    ],
    error: null,
    fold: false,
  },
  {
    name: "unknown",
    input: { task_ids: ["unknown"] },
    reports: null,
    error: null,
    fold: false,
  },
  {
    name: "known mismatch",
    input: { task_ids: ["run-1"] },
    reports: [{ task_id: "run-2", status: "running" }],
    error: null,
    fold: false,
  },
  {
    name: "invalid structured report",
    input: { task_ids: ["run-1"] },
    reports: [{ status: "running" }],
    error: null,
    fold: false,
  },
  { name: "empty union", input: {}, reports: null, error: null, fold: false },
] as const

function statusPoll(
  toolCallId: string,
  input: Record<string, unknown> | null,
  reports: readonly Record<string, unknown>[] | null,
  errorText: string | null
): AdaptedToolCallPart {
  return {
    type: "tool-call",
    toolCallId,
    toolName: "get_delegation_status",
    input: input === null ? null : JSON.stringify(input),
    output:
      reports === null
        ? null
        : JSON.stringify({ structuredContent: { tasks: reports } }),
    ...(errorText ? { errorText } : {}),
    state: errorText ? "output-error" : "output-available",
  }
}

function statusGroup(poll: AdaptedToolCallPart): AdaptedContentPart {
  return { type: "delegation-status-group", polls: [poll] }
}

function text(value: string): AdaptedContentPart {
  return { type: "text", text: value }
}

function allParts(messages: readonly AdaptedMessage[]): AdaptedContentPart[] {
  return messages.flatMap((message) => message.content)
}

function workUnits(
  messages: readonly AdaptedMessage[]
): AdaptedDelegationWorkUnitPart[] {
  return allParts(messages).filter(
    (part): part is AdaptedDelegationWorkUnitPart =>
      part.type === "delegation-work-unit"
  )
}

function visibleStatusTaskIds(messages: readonly AdaptedMessage[]): string[] {
  return allParts(messages).flatMap((part) => {
    if (part.type !== "delegation-status-group") return []
    const visible = part.visibleTaskIds
      ? new Set(part.visibleTaskIds)
      : undefined
    return buildDelegationTaskRows(part.polls, visible)
      .map((row) => row.taskId)
      .filter((taskId): taskId is string => taskId !== null)
  })
}

function projectedStatusPollIds(messages: readonly AdaptedMessage[]): string[] {
  return allParts(messages).flatMap((part) =>
    part.type === "delegation-status-group"
      ? part.polls.map((poll) => poll.toolCallId)
      : []
  )
}

function assistantText(messages: readonly AdaptedMessage[]): string {
  return allParts(messages)
    .filter(
      (part): part is Extract<AdaptedContentPart, { type: "text" }> =>
        part.type === "text"
    )
    .map((part) => part.text)
    .join("\n")
}

function conversation2582Messages(): AdaptedMessage[] {
  const taskId = "81cb187d-1473-43bf-be66-43072f554407"
  return [
    assistant(
      "1",
      delegate("delegate-call-1", taskId, "conversation-2582-run")
    ),
    assistant(
      "2",
      statusGroup(
        statusPoll(
          "status-timeout-1",
          { task_ids: [taskId], wait_ms: 0 },
          null,
          "timed out awaiting tools/call after 300s"
        )
      )
    ),
    assistant("3", text("continuation checkpoint")),
    assistant(
      "4",
      statusGroup(
        statusPoll(
          "status-timeout-2",
          { task_ids: [taskId], wait_ms: 0 },
          null,
          "timed out awaiting tools/call after 300s"
        )
      )
    ),
    assistant(
      "5",
      statusGroup(
        statusPoll(
          "status-running-3",
          { task_ids: [taskId], wait_ms: 0 },
          [{ task_id: taskId, status: "running" }],
          null
        )
      )
    ),
  ]
}

describe("projectDelegationTranscript", () => {
  it("projects a multi-turn history while keeping an indivisible mixed call", () => {
    const messages = [
      assistant("1", delegate("tool-1", "run-1", "unit-a")),
      assistant(
        "2",
        status("poll-1", [{ taskId: "run-1", status: "running" }])
      ),
      assistant("3", text("checkpoint explanation")),
      assistant(
        "4",
        delegate("tool-2", "run-2", "unit-a", {
          toolName: "continue_delegation",
          targetTaskId: "run-1",
        })
      ),
      assistant(
        "5",
        status("poll-2", [{ taskId: "run-2", status: "running" }])
      ),
      assistant("6", text("still working")),
      assistant(
        "7",
        status("poll-3", [{ taskId: "run-2", status: "completed" }])
      ),
      assistant(
        "8",
        status("poll-4", [
          { taskId: "run-2", status: "completed" },
          { taskId: "unknown-run", status: "completed" },
        ])
      ),
    ]
    const originalParts = allParts(messages)

    const projected = projectDelegationTranscript(messages, 2075)

    expect(workUnits(projected.messages)).toHaveLength(2)
    expect(workUnits(projected.messages).map((unit) => unit.key)).toEqual([
      "wu:unit-a:run-1",
      "wu:unit-a:run-2",
    ])
    expect(
      workUnits(projected.messages).map((unit) =>
        unit.sources.map((source) => source.toolCallId)
      )
    ).toEqual([["tool-1"], ["tool-2"]])
    // Each card sits at its original turn position (not collapsed to the first).
    expect(projected.messages[0].content[0]).toMatchObject({
      type: "delegation-work-unit",
      key: "wu:unit-a:run-1",
    })
    expect(projected.messages[3].content[0]).toMatchObject({
      type: "delegation-work-unit",
      key: "wu:unit-a:run-2",
    })
    expect(
      allParts(projected.messages)
        .filter((part) => part.type === "text")
        .map((part) => part.text)
    ).toEqual(["checkpoint explanation", "still working"])
    expect(visibleStatusTaskIds(projected.messages)).toEqual([
      "run-2",
      "unknown-run",
    ])
    expect(projected.messages[2]).toBe(messages[2])
    expect(projected.messages[5]).toBe(messages[5])
    expect(originalParts).toEqual(allParts(messages))
    expect(originalParts).not.toContainEqual(
      expect.objectContaining({ type: "delegation-work-unit" })
    )
  })

  it("keeps parallel explicit work units separate", () => {
    const projected = projectDelegationTranscript(
      [
        assistant(
          "1",
          delegate("a", "run-a", "unit-a", { childConversationId: 10 })
        ),
        assistant(
          "2",
          delegate("b", "run-b", "unit-b", { childConversationId: 20 })
        ),
        assistant(
          "3",
          status("poll", [
            { taskId: "run-a", status: "running" },
            { taskId: "run-b", status: "running" },
          ])
        ),
      ],
      2075
    )

    expect(workUnits(projected.messages).map((unit) => unit.key)).toEqual([
      "wu:unit-a:run-a",
      "wu:unit-b:run-b",
    ])
    expect(visibleStatusTaskIds(projected.messages)).toEqual([])
  })

  it("resolves out-of-order task linkage while keeping one card per run", () => {
    const projected = projectDelegationTranscript(
      [
        assistant(
          "1",
          delegate("continue", "run-2", null, {
            toolName: "continue_delegation",
            targetTaskId: "run-1",
          })
        ),
        assistant("2", delegate("initial", "run-1", null)),
      ],
      2075
    )

    expect(workUnits(projected.messages)).toHaveLength(2)
    expect(
      workUnits(projected.messages).map((unit) =>
        unit.sources.map((source) => source.toolCallId)
      )
    ).toEqual([["continue"], ["initial"]])
    // Status folding still knows both task ids belong to the linked unit.
    expect(projected.identityIndex.knownTaskIds.has("run-1")).toBe(true)
    expect(projected.identityIndex.knownTaskIds.has("run-2")).toBe(true)
  })

  it("retains wholly unmapped status groups by reference", () => {
    const statusPart = status("poll", [
      { taskId: "unknown", status: "running" },
    ])
    const statusMessage = assistant("2", statusPart)
    const projected = projectDelegationTranscript(
      [assistant("1", delegate("a", "run-a", "unit-a")), statusMessage],
      2075
    )

    expect(projected.messages[1]).toBe(statusMessage)
    expect(projected.messages[1].content[0]).toBe(statusPart)
  })

  it.each(statusCases)("applies whole-call identity rule to $name", (entry) => {
    const poll = statusPoll(entry.name, entry.input, entry.reports, entry.error)
    const historical = projectDelegationTranscript(
      [
        assistant("1", delegate("delegate-1", "run-1", "unit-a")),
        assistant("2", delegate("delegate-2", "run-2", "unit-b")),
        assistant("3", statusGroup(poll)),
      ],
      2582
    )
    expect(
      projectedStatusPollIds(historical.messages).includes(entry.name)
    ).toBe(!entry.fold)
    expect(
      shouldFoldLiveDelegationTool(poll, historical.identityIndex, 2582)
    ).toBe(entry.fold)
  })

  it("fails open on malformed identity-bearing input", () => {
    const poll: AdaptedToolCallPart = {
      type: "tool-call",
      toolCallId: "malformed-input",
      toolName: "get_delegation_status",
      input: '{"task_ids":[',
      output: null,
      state: "output-error",
    }
    const projected = projectDelegationTranscript(
      [
        assistant("1", delegate("delegate-1", "run-1", "unit-a")),
        assistant("2", statusGroup(poll)),
      ],
      2582
    )
    expect(projectedStatusPollIds(projected.messages)).toEqual([
      "malformed-input",
    ])
    expect(
      shouldFoldLiveDelegationTool(poll, projected.identityIndex, 2582)
    ).toBe(false)
  })

  it("fails open when an exact id belongs to two distinct runs", () => {
    const poll = statusPoll(
      "ambiguous-run",
      { task_ids: ["duplicate"] },
      [{ task_id: "duplicate", status: "running" }],
      null
    )
    const projected = projectDelegationTranscript(
      [
        assistant("1", delegate("delegate-1", "duplicate", "unit-a")),
        assistant("2", delegate("delegate-2", "duplicate", "unit-a")),
        assistant("3", statusGroup(poll)),
      ],
      2582
    )
    expect(projectedStatusPollIds(projected.messages)).toEqual([
      "ambiguous-run",
    ])
    expect(
      shouldFoldLiveDelegationTool(poll, projected.identityIndex, 2582)
    ).toBe(false)
  })

  it("keeps every row from an indivisible mixed known-unknown call", () => {
    const mixed = statusPoll(
      "mixed",
      { task_ids: ["run-1", "unknown"] },
      [
        { task_id: "run-1", status: "running" },
        { task_id: "unknown", status: "running" },
      ],
      null
    )
    const projected = projectDelegationTranscript(
      [
        assistant("1", delegate("delegate", "run-1", "unit-a")),
        assistant("2", statusGroup(mixed)),
      ],
      2582
    )
    expect(visibleStatusTaskIds(projected.messages)).toEqual([
      "run-1",
      "unknown",
    ])
  })

  it("folds conversation 2582 polls independently of call id and checkpoints", () => {
    const projected = projectDelegationTranscript(
      conversation2582Messages(),
      2582
    )

    expect(workUnits(projected.messages)).toHaveLength(1)
    expect(workUnits(projected.messages)[0].sources).toHaveLength(1)
    expect(visibleStatusTaskIds(projected.messages)).toEqual([])
    expect(assistantText(projected.messages)).toContain(
      "continuation checkpoint"
    )
  })

  it("marks mapped successful cancellation without removing its audit card", () => {
    const cancel: AdaptedToolCallPart = {
      type: "tool-call",
      toolCallId: "cancel-1",
      toolName: "cancel_delegation",
      input: JSON.stringify({ task_id: "run-1" }),
      output: JSON.stringify({
        status: "canceled",
        task_id: "run-1",
        error_code: "user_cancelled",
      }),
      state: "output-available",
    }
    const projected = projectDelegationTranscript(
      [
        assistant("1", delegate("a", "run-1", "unit-a")),
        assistant("2", cancel),
      ],
      2075
    )

    expect(workUnits(projected.messages)[0].explicitUserCancel).toBe(true)
    expect(allParts(projected.messages)).toContain(cancel)
  })

  it("does not mark timeout guidance as an explicit user cancellation", () => {
    const cancel: AdaptedToolCallPart = {
      type: "tool-call",
      toolCallId: "cancel-1",
      toolName: "cancel_delegation",
      input: JSON.stringify({ task_id: "run-1", reason: "timeout" }),
      output: JSON.stringify({
        status: "running",
        task_id: "run-1",
        message: "Use get_delegation_status to keep waiting",
      }),
      state: "output-available",
    }
    const projected = projectDelegationTranscript(
      [
        assistant("1", delegate("a", "run-1", "unit-a")),
        assistant("2", cancel),
      ],
      2075
    )

    expect(workUnits(projected.messages)[0].explicitUserCancel).toBe(false)
  })

  it("recurses through goal-run items without changing goal chrome", () => {
    const start = delegate("goal-start-shape", "unused", null)
    start.toolName = "create_goal"
    const goal: AdaptedContentPart = {
      type: "goal-run",
      start,
      end: null,
      items: [delegate("nested", "run-1", "unit-a")],
      isRunning: false,
    }
    const projected = projectDelegationTranscript([assistant("1", goal)], 2075)

    expect(projected.messages[0].content[0]).toMatchObject({
      type: "goal-run",
      start,
      end: null,
      isRunning: false,
    })
    const projectedGoal = projected.messages[0].content[0]
    expect(projectedGoal.type).toBe("goal-run")
    if (projectedGoal.type === "goal-run") {
      expect(projectedGoal.items[0].type).toBe("delegation-work-unit")
    }
  })
})

describe("shouldFoldLiveDelegationTool", () => {
  const projected = projectDelegationTranscript(
    [assistant("1", delegate("a", "run-1", "unit-a"))],
    2075
  )

  it("folds only status calls whose non-empty ids are all known", () => {
    const known = (
      status("known", [{ taskId: "run-1", status: "running" }]) as Extract<
        AdaptedContentPart,
        { type: "delegation-status-group" }
      >
    ).polls[0]
    const mixed = (
      status("mixed", [
        { taskId: "run-1", status: "running" },
        { taskId: "unknown", status: "running" },
      ]) as Extract<AdaptedContentPart, { type: "delegation-status-group" }>
    ).polls[0]

    expect(
      shouldFoldLiveDelegationTool(known, projected.identityIndex, 2075)
    ).toBe(true)
    expect(
      shouldFoldLiveDelegationTool(mixed, projected.identityIndex, 2075)
    ).toBe(false)
  })

  it("never folds live delegate/continue tool calls into historical cards", () => {
    const continuation = delegate("continue", "run-2", null, {
      toolName: "continue_delegation",
      targetTaskId: "run-1",
    })
    const initial = delegate("new", "run-new", null)
    initial.output = null

    expect(
      shouldFoldLiveDelegationTool(continuation, projected.identityIndex, 2075)
    ).toBe(false)
    expect(
      shouldFoldLiveDelegationTool(initial, projected.identityIndex, 2075)
    ).toBe(false)
  })

  it("does not parse unrelated live tools as delegation input", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {})
    const shell: AdaptedToolCallPart = {
      type: "tool-call",
      toolCallId: "shell",
      toolName: "shell",
      input: JSON.stringify({ command: "git status" }),
      state: "input-available",
    }

    try {
      expect(
        shouldFoldLiveDelegationTool(shell, projected.identityIndex, 2075)
      ).toBe(false)
      expect(warn).not.toHaveBeenCalled()
    } finally {
      warn.mockRestore()
    }
  })
})
