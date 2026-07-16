import { describe, expect, it } from "vitest"

import {
  groupConsecutiveBackgroundTasks,
  groupConsecutiveToolCalls,
  mergeAdjacentBackgroundTaskGroups,
  type AdaptedContentPart,
  type AdaptedToolCallPart,
} from "@/lib/adapters/ai-elements-adapter"
import { deriveNativeActivitiesFromToolCalls } from "@/lib/delegation-activity"

const POLL_OUTPUT = `<retrieval_status>success</retrieval_status>
<task_id>bfb5xnq1t</task_id>
<task_type>local_bash</task_type>
<status>completed</status>
<exit_code>0</exit_code>
<output>done</output>`

function taskPoll(id: string): AdaptedToolCallPart {
  return {
    type: "tool-call",
    toolCallId: id,
    toolName: "TaskOutput",
    input: JSON.stringify({ task_id: "bfb5xnq1t", block: true, timeout: 1000 }),
    output: POLL_OUTPUT,
    state: "output-available",
  }
}

function bash(id: string): AdaptedToolCallPart {
  return {
    type: "tool-call",
    toolCallId: id,
    toolName: "bash",
    input: JSON.stringify({ command: "ls" }),
    output: "file.txt",
    state: "output-available",
  }
}

const text = (s: string): AdaptedContentPart => ({ type: "text", text: s })

describe("groupConsecutiveBackgroundTasks", () => {
  it("collapses a consecutive run of polls into one group", () => {
    const parts = groupConsecutiveBackgroundTasks([
      text("before"),
      taskPoll("p1"),
      taskPoll("p2"),
      text("after"),
    ])
    expect(parts.map((p) => p.type)).toEqual([
      "text",
      "background-task-group",
      "text",
    ])
    const group = parts[1]
    expect(group.type === "background-task-group" && group.polls).toHaveLength(
      2
    )
  })

  it("does NOT merge polls separated by other content", () => {
    const parts = groupConsecutiveBackgroundTasks([
      taskPoll("p1"),
      text("interruption"),
      taskPoll("p2"),
    ])
    expect(parts.map((p) => p.type)).toEqual([
      "background-task-group",
      "text",
      "background-task-group",
    ])
  })
})

describe("groupConsecutiveToolCalls + background tasks", () => {
  it("leaves background-task polls standalone (out of the tool-group)", () => {
    const parts = groupConsecutiveToolCalls([bash("b1"), taskPoll("p1")])
    // bash folds into a tool-group; the poll breaks out as a bare tool-call.
    expect(parts.map((p) => p.type)).toEqual(["tool-group", "tool-call"])
    expect(parts[1].type === "tool-call" && parts[1].toolName).toBe(
      "TaskOutput"
    )
  })
})

describe("mergeAdjacentBackgroundTaskGroups", () => {
  it("merges adjacent groups (cross-turn poll rounds)", () => {
    const parts = mergeAdjacentBackgroundTaskGroups([
      { type: "background-task-group", polls: [taskPoll("p1")] },
      { type: "background-task-group", polls: [taskPoll("p2")] },
    ])
    expect(parts).toHaveLength(1)
    expect(
      parts[0].type === "background-task-group" && parts[0].polls
    ).toHaveLength(2)
  })

  it("does not merge groups separated by other parts", () => {
    const parts = mergeAdjacentBackgroundTaskGroups([
      { type: "background-task-group", polls: [taskPoll("p1")] },
      text("x"),
      { type: "background-task-group", polls: [taskPoll("p2")] },
    ])
    expect(parts).toHaveLength(3)
  })
})

/**
 * Historical integration: Agent spawn + grouped TaskOutput/TaskStop must both
 * project as activities while the original background-task-group remains for
 * rendering (I2). Mirrors message-list historical walk after adapter grouping.
 */
describe("historical background-task-group → native activity projection", () => {
  function agentSpawn(): AdaptedToolCallPart {
    return {
      type: "tool-call",
      toolCallId: "agent-1",
      toolName: "Agent",
      input: JSON.stringify({
        subagent_type: "Explore",
        description: "investigate",
        prompt: "find bugs",
      }),
      output: JSON.stringify({ task_id: "bfb5xnq1t" }),
      state: "output-available",
    }
  }

  function taskStop(id: string): AdaptedToolCallPart {
    return {
      type: "tool-call",
      toolCallId: id,
      toolName: "TaskStop",
      input: JSON.stringify({ task_id: "bfb5xnq1t" }),
      output: JSON.stringify({
        message: "Successfully stopped task: bfb5xnq1t (sleep 10)",
        task_id: "bfb5xnq1t",
        task_type: "local_bash",
        command: "sleep 10",
      }),
      state: "output-available",
    }
  }

  /** Same walk as message-list-view lastAssistantActivities historical path. */
  function collectToolsFromAdaptedParts(parts: AdaptedContentPart[]) {
    const tools: Array<{
      toolCallId: string
      toolName: string
      input?: string | null
      output?: string | null
      status?: string | null
    }> = []
    const walk = (items: AdaptedContentPart[]) => {
      for (const part of items) {
        if (part.type === "tool-call" && part.toolCallId) {
          tools.push({
            toolCallId: part.toolCallId,
            toolName: part.toolName,
            input: part.input ?? null,
            output: part.output ?? part.errorText ?? null,
            status:
              part.state === "output-error"
                ? "failed"
                : part.state === "output-available"
                  ? "completed"
                  : "in_progress",
          })
        } else if (part.type === "tool-group") {
          walk(part.items)
        } else if (part.type === "background-task-group") {
          for (const poll of part.polls) {
            if (!poll.toolCallId) continue
            tools.push({
              toolCallId: poll.toolCallId,
              toolName: poll.toolName,
              input: poll.input ?? null,
              output: poll.output ?? poll.errorText ?? null,
              status:
                poll.state === "output-error"
                  ? "failed"
                  : poll.state === "output-available"
                    ? "completed"
                    : "in_progress",
            })
          }
        }
      }
    }
    walk(parts)
    return tools
  }

  it("projects Agent + TaskOutput wait + TaskStop cancel after grouping", () => {
    const raw: AdaptedContentPart[] = [
      text("delegating"),
      agentSpawn(),
      taskPoll("poll-1"),
      taskStop("stop-1"),
    ]
    // Production adapter pipeline: group tool calls then background tasks.
    const grouped = groupConsecutiveBackgroundTasks(
      groupConsecutiveToolCalls(raw)
    )

    // Original group card still present for rendering — not flattened away.
    expect(grouped.some((p) => p.type === "background-task-group")).toBe(true)
    const bgGroup = grouped.find((p) => p.type === "background-task-group")
    expect(
      bgGroup && bgGroup.type === "background-task-group" && bgGroup.polls
    ).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ toolName: "TaskOutput" }),
        expect.objectContaining({ toolName: "TaskStop" }),
      ])
    )
    // Agent remains a plain tool-call (not consumed).
    expect(
      grouped.some((p) => p.type === "tool-call" && p.toolName === "Agent")
    ).toBe(true)

    const tools = collectToolsFromAdaptedParts(grouped)
    // Without walking the group, only Agent would appear (polls lost) — prove
    // TaskOutput/TaskStop enter the tool list:
    expect(tools.map((t) => t.toolName)).toEqual(
      expect.arrayContaining(["Agent", "TaskOutput", "TaskStop"])
    )

    const activities = deriveNativeActivitiesFromToolCalls(tools, "claude_code")

    // Same task_id merges spawn→wait→cancel into one lifecycle row; final op
    // is cancel after TaskStop (not dropped after grouping).
    expect(activities.length).toBeGreaterThanOrEqual(1)
    const row = activities.find((a) => a.task_id === "bfb5xnq1t")
    expect(row).toBeDefined()
    expect(row).toMatchObject({
      origin: "native",
      authoritative: false,
      platform: "claude_code",
      operation: "cancel",
    })

    // Intermediate: Agent+TaskOutput only (no stop) projects wait.
    const waitOnly = deriveNativeActivitiesFromToolCalls(
      collectToolsFromAdaptedParts(
        groupConsecutiveBackgroundTasks(
          groupConsecutiveToolCalls([agentSpawn(), taskPoll("poll-1")])
        )
      ),
      "claude_code"
    )
    expect(waitOnly.some((a) => a.operation === "wait")).toBe(true)
  })

  it("does not project Agent/Task without platform hint", () => {
    const tools = collectToolsFromAdaptedParts([
      agentSpawn(),
      { type: "background-task-group", polls: [taskPoll("p1")] },
    ])
    const activities = deriveNativeActivitiesFromToolCalls(tools, null)
    // TaskOutput is exclusive to claude_code table → still projects wait.
    // Agent is ambiguous → skipped without hint.
    expect(activities.every((a) => a.operation !== "spawn")).toBe(true)
    expect(activities.some((a) => a.operation === "wait")).toBe(true)
  })
})
