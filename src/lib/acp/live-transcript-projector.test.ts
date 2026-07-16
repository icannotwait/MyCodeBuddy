import { describe, expect, it } from "vitest"
import type {
  LiveMessage,
  ToolCallInfo,
} from "@/contexts/acp-connections-context"
import type { EventEnvelope, PlanEntryInfo } from "@/lib/types"
import { buildStreamingTurnsFromLiveMessage } from "@/stores/conversation-runtime-store"
import {
  applyEventsToCanonicalLiveMessage,
  applyLiveTranscriptEvents,
  liveTranscriptToCanonicalMessage,
  projectLiveSnapshot,
} from "./live-transcript-projector"

function liveMessageWithText(
  text: string,
  id = "msg-1",
  startedAt = 1_000
): LiveMessage {
  return {
    id,
    role: "assistant",
    content: text.length > 0 ? [{ type: "text", text }] : [],
    startedAt,
  }
}

function envelope(
  seq: number,
  connectionId: string,
  payload: Omit<EventEnvelope, "seq" | "connection_id">
): EventEnvelope {
  return {
    connection_id: connectionId,
    seq,
    ...payload,
  } as EventEnvelope
}

function toolCreate(
  connectionId: string,
  seq: number,
  toolCallId: string,
  overrides: Partial<{
    title: string
    kind: string
    status: string
    content: string | null
    raw_input: string | null
    raw_output: string | null
    meta: Record<string, unknown> | null
    images: ToolCallInfo["images"]
  }> = {}
): EventEnvelope {
  return envelope(seq, connectionId, {
    type: "tool_call",
    tool_call_id: toolCallId,
    title: overrides.title ?? "Bash",
    kind: overrides.kind ?? "execute",
    status: overrides.status ?? "pending",
    content: overrides.content ?? null,
    raw_input: overrides.raw_input ?? "{}",
    raw_output: overrides.raw_output ?? null,
    meta: overrides.meta ?? null,
    images: overrides.images,
  })
}

function toolUpdate(
  connectionId: string,
  seq: number,
  toolCallId: string,
  status: string,
  overrides: Partial<{
    title: string | null
    content: string | null
    raw_input: string | null
    raw_output: string | null
    raw_output_append: boolean
    meta: Record<string, unknown> | null
    images: ToolCallInfo["images"]
  }> = {}
): EventEnvelope {
  return envelope(seq, connectionId, {
    type: "tool_call_update",
    tool_call_id: toolCallId,
    title: overrides.title ?? null,
    status,
    content: overrides.content ?? null,
    raw_input: overrides.raw_input ?? null,
    raw_output: overrides.raw_output ?? null,
    raw_output_append: overrides.raw_output_append,
    meta: overrides.meta ?? null,
    images: overrides.images,
  })
}

function planEntries(
  items: Array<{ content: string; status?: string; priority?: string }>
): PlanEntryInfo[] {
  return items.map((item) => ({
    content: item.content,
    status: item.status ?? "pending",
    priority: item.priority ?? "medium",
  }))
}

const agentLiveFixtures: Array<{
  name: string
  conversationId: number
  snapshot: LiveMessage
  events: EventEnvelope[]
}> = [
  {
    name: "Claude text/thinking/tools",
    conversationId: 1,
    snapshot: {
      id: "claude-1",
      role: "assistant",
      content: [],
      startedAt: 10,
    },
    events: [
      envelope(1, "c1", { type: "thinking", text: "Let me reason" }),
      envelope(2, "c1", { type: "content_delta", text: "Hello" }),
      envelope(3, "c1", { type: "content_delta", text: " world" }),
      toolCreate("c1", 4, "tool-read", {
        title: "Read",
        kind: "read",
        status: "in_progress",
        raw_input: JSON.stringify({ path: "a.ts" }),
      }),
      toolUpdate("c1", 5, "tool-read", "completed", {
        content: "file contents",
      }),
      envelope(6, "c1", { type: "content_delta", text: " done" }),
    ],
  },
  {
    name: "Codex child-agent metadata and generated images",
    conversationId: 2,
    snapshot: {
      id: "codex-1",
      role: "assistant",
      content: [],
      startedAt: 20,
    },
    events: [
      envelope(1, "c1", { type: "content_delta", text: "Generating" }),
      toolCreate("c1", 2, "agent-1", {
        title: "Agent",
        kind: "other",
        status: "in_progress",
        raw_input: JSON.stringify({ description: "subtask" }),
        meta: { codex: { collab: true } },
      }),
      toolCreate("c1", 3, "child-read", {
        title: "Read",
        kind: "read",
        status: "completed",
        raw_input: JSON.stringify({ path: "img.png" }),
        meta: { claudeCode: { parentToolUseId: "agent-1" } },
      }),
      toolUpdate("c1", 4, "agent-1", "completed"),
      toolCreate("c1", 5, "img-1", {
        title: "Image generation",
        kind: "other",
        status: "in_progress",
        content: "Revised prompt: a cat",
        raw_input: null,
      }),
      toolUpdate("c1", 6, "img-1", "completed", {
        content: "Revised prompt: a cat",
        images: [
          {
            data: "base64img",
            mime_type: "image/png",
            uri: "~/.codex/generated_images/cat.png",
          },
        ],
      }),
    ],
  },
  {
    name: "CodeBuddy delegation metadata",
    conversationId: 3,
    snapshot: {
      id: "cb-1",
      role: "assistant",
      content: [],
      startedAt: 30,
    },
    events: [
      envelope(1, "c1", { type: "content_delta", text: "Delegating" }),
      toolCreate("c1", 2, "agent-cb", {
        title: "Agent",
        kind: "other",
        status: "in_progress",
        raw_input: JSON.stringify({ prompt: "work" }),
      }),
      toolCreate("c1", 3, "child-bash", {
        title: "Bash",
        kind: "execute",
        status: "completed",
        raw_input: JSON.stringify({ command: "ls" }),
        meta: { "codebuddy.ai/parentToolCallId": "agent-cb" },
      }),
      toolUpdate("c1", 4, "agent-cb", "completed", {
        content: "child finished",
      }),
    ],
  },
  {
    name: "Kimi plan replacement",
    conversationId: 4,
    snapshot: {
      id: "kimi-1",
      role: "assistant",
      content: [],
      startedAt: 40,
    },
    events: [
      envelope(1, "c1", { type: "content_delta", text: "Planning" }),
      toolCreate("c1", 2, "todo-1", {
        title: "Updating todo list",
        kind: "other",
        status: "in_progress",
        raw_input: JSON.stringify({
          todos: [
            { title: "A", status: "in_progress" },
            { title: "B", status: "pending" },
          ],
        }),
      }),
      envelope(3, "c1", {
        type: "plan_update",
        entries: planEntries([
          { content: "A", status: "in_progress" },
          { content: "B", status: "pending" },
        ]),
      }),
      toolCreate("c1", 4, "todo-2", {
        title: "Updating todo list",
        kind: "other",
        status: "completed",
        raw_input: JSON.stringify({
          todos: [
            { title: "A", status: "completed" },
            { title: "B", status: "in_progress" },
            { title: "C", status: "pending" },
          ],
        }),
      }),
      envelope(5, "c1", {
        type: "plan_update",
        entries: planEntries([
          { content: "A", status: "completed" },
          { content: "B", status: "in_progress" },
          { content: "C", status: "pending" },
        ]),
      }),
      envelope(6, "c1", { type: "content_delta", text: " next" }),
    ],
  },
  {
    name: "Grok rich text/tool appends",
    conversationId: 5,
    snapshot: {
      id: "grok-1",
      role: "assistant",
      content: [{ type: "text", text: "# Title\n\n" }],
      startedAt: 50,
    },
    events: [
      envelope(1, "c1", {
        type: "content_delta",
        text: "```ts\nconst x = 1\n```\n\n",
      }),
      toolCreate("c1", 2, "bash-1", {
        title: "Bash",
        kind: "execute",
        status: "in_progress",
        raw_input: JSON.stringify({ command: "echo hi" }),
      }),
      toolUpdate("c1", 3, "bash-1", "in_progress", {
        raw_output: "hi\n",
        raw_output_append: true,
      }),
      toolUpdate("c1", 4, "bash-1", "in_progress", {
        raw_output: "more\n",
        raw_output_append: true,
      }),
      toolUpdate("c1", 5, "bash-1", "completed", {
        raw_output: "done\n",
        raw_output_append: true,
      }),
      envelope(6, "c1", { type: "content_delta", text: "Finished." }),
    ],
  },
]

describe("live-transcript-projector", () => {
  it("keeps segment ids stable for text append and isolates tool updates", () => {
    let projection = projectLiveSnapshot(
      42,
      "c1",
      liveMessageWithText("hello"),
      1
    )
    const ids = projection.segmentIds
    const firstText = projection.segments.get(ids[0])

    projection = applyLiveTranscriptEvents(projection, [
      envelope(2, "c1", { type: "content_delta", text: " world" }),
    ])
    expect(projection.segmentIds).toBe(ids)
    expect(projection.segments.get(ids[0])).not.toBe(firstText)
    expect(projection.segments.get(ids[0])).toMatchObject({
      type: "text",
      text: "hello world",
    })

    projection = applyLiveTranscriptEvents(projection, [
      toolCreate("c1", 3, "t1"),
    ])
    const idsAfterTool = projection.segmentIds
    const textAfterTool = projection.segments.get(ids[0])
    projection = applyLiveTranscriptEvents(projection, [
      toolUpdate("c1", 4, "t1", "done"),
    ])
    expect(projection.segmentIds).toBe(idsAfterTool)
    expect(projection.segments.get(ids[0])).toBe(textAfterTool)
    expect(projection.tools.get("t1")?.status).toBe("done")
  })

  it("preserves plan content order on full snapshot rebuild", () => {
    const projection = projectLiveSnapshot(
      1,
      "c1",
      {
        id: "m",
        role: "assistant",
        content: [
          { type: "text", text: "before" },
          {
            type: "plan",
            entries: planEntries([{ content: "step" }]),
          },
          { type: "text", text: "after" },
        ],
        startedAt: 1,
      },
      0
    )
    const planId = `${projection.messageId}:plan:0`
    expect(projection.segmentIds).toEqual([
      `${projection.messageId}:text:0`,
      planId,
      `${projection.messageId}:text:1`,
    ])
    expect(projection.segments.get(planId)).toMatchObject({
      type: "plan",
      entries: [{ content: "step" }],
    })
  })

  it("moves the stable plan id to the end on plan_update", () => {
    let projection = projectLiveSnapshot(
      1,
      "c1",
      {
        id: "m",
        role: "assistant",
        content: [
          { type: "text", text: "hi" },
          {
            type: "plan",
            entries: planEntries([{ content: "old" }]),
          },
        ],
        startedAt: 1,
      },
      0
    )
    const planId = `${projection.messageId}:plan:0`
    expect(projection.segmentIds[projection.segmentIds.length - 1]).toBe(planId)

    projection = applyLiveTranscriptEvents(projection, [
      envelope(1, "c1", { type: "content_delta", text: "!" }),
    ])
    // Text append after plan should create a new text segment after plan?
    // Rule: append to final text only when structurally last. Plan is last,
    // so content_delta adds a new text segment (and plan is not reordered).
    expect(projection.segmentIds).toContain(planId)

    projection = applyLiveTranscriptEvents(projection, [
      envelope(2, "c1", {
        type: "plan_update",
        entries: planEntries([{ content: "new" }]),
      }),
    ])
    expect(projection.segmentIds[projection.segmentIds.length - 1]).toBe(planId)
    expect(projection.segments.get(planId)).toMatchObject({
      type: "plan",
      entries: [{ content: "new" }],
    })
  })

  it.each(agentLiveFixtures)(
    "matches canonical completed turns for $name",
    ({ conversationId, snapshot, events }) => {
      let projection = projectLiveSnapshot(conversationId, "c1", snapshot, 0)
      projection = applyLiveTranscriptEvents(projection, events)
      const projectedCanonical = liveTranscriptToCanonicalMessage(projection)
      expect(
        buildStreamingTurnsFromLiveMessage(conversationId, projectedCanonical)
      ).toEqual(
        buildStreamingTurnsFromLiveMessage(
          conversationId,
          applyEventsToCanonicalLiveMessage(snapshot, events)
        )
      )
    }
  )
})
