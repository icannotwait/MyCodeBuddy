import { describe, expect, it } from "vitest"

import type {
  AdaptedContentPart,
  AdaptedToolCallPart,
} from "@/lib/adapters/ai-elements-adapter"
import {
  filterDelegatedInterruptParts,
  isConversationInterruptedAgentText,
} from "@/lib/delegation-conversation-interrupted"

describe("isConversationInterruptedAgentText", () => {
  it.each([
    "Conversation interrupted",
    "*Conversation interrupted*",
    " **Conversation interrupted** \n",
    "__Conversation interrupted__",
    "_Conversation interrupted_",
  ])("matches the exact interruption marker %j", (text) => {
    expect(isConversationInterruptedAgentText(text)).toBe(true)
  })

  it.each([
    "*Conversation interrupted*\nMore detail",
    "Conversation was interrupted",
    "Conversation interrupted by user",
    "`Conversation interrupted`",
  ])("keeps non-exact assistant text %j", (text) => {
    expect(isConversationInterruptedAgentText(text)).toBe(false)
  })
})

describe("filterDelegatedInterruptParts", () => {
  const goalStart: AdaptedToolCallPart = {
    type: "tool-call",
    toolCallId: "goal-1",
    toolName: "update_goal",
    input: null,
    state: "output-available",
  }

  it("filters only assistant display text, including nested goal-run items", () => {
    const parts: AdaptedContentPart[] = [
      { type: "text", text: "*Conversation interrupted*" },
      { type: "text", text: "Conversation interrupted by user" },
      {
        type: "reasoning",
        content: "Conversation interrupted",
        isStreaming: false,
      },
      {
        type: "tool-result",
        toolCallId: "tool-1",
        output: "Conversation interrupted",
        state: "output-available",
      },
      {
        type: "goal-run",
        start: goalStart,
        end: null,
        items: [
          { type: "text", text: "Conversation interrupted" },
          { type: "text", text: "nested result stays" },
        ],
        isRunning: false,
      },
    ]

    const filtered = filterDelegatedInterruptParts(parts, true)

    expect(filtered).not.toBe(parts)
    expect(filtered).toContainEqual({
      type: "text",
      text: "Conversation interrupted by user",
    })
    expect(filtered).toContainEqual(
      expect.objectContaining({
        type: "reasoning",
        content: "Conversation interrupted",
      })
    )
    expect(filtered).toContainEqual(
      expect.objectContaining({
        type: "tool-result",
        output: "Conversation interrupted",
      })
    )
    const goal = filtered.find((part) => part.type === "goal-run")
    expect(goal?.type).toBe("goal-run")
    if (goal?.type !== "goal-run") throw new Error("expected goal run")
    expect(goal.items).toEqual([{ type: "text", text: "nested result stays" }])
  })

  it("returns the original array for standalone conversations", () => {
    const parts: AdaptedContentPart[] = [
      { type: "text", text: "Conversation interrupted" },
    ]
    expect(filterDelegatedInterruptParts(parts, false)).toBe(parts)
  })
})
