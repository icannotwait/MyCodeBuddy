import { describe, expect, it } from "vitest"

import type {
  AdaptedContentPart,
  AdaptedToolCallPart,
} from "@/lib/adapters/ai-elements-adapter"
import {
  filterCodexCompactionAdvisoryParts,
  isCodexCompactionAdvisoryText,
} from "@/lib/codex-compaction-advisory"

const ADVISORY =
  "Heads up: Long threads and multiple compactions can cause the model to be less accurate. Start a new thread when possible to keep threads small and targeted."

describe("isCodexCompactionAdvisoryText", () => {
  it.each([
    ADVISORY,
    `Warning: ${ADVISORY}`,
    `  ${ADVISORY}  \n`,
    `Warning: ${ADVISORY}\n\n`,
  ])("matches the exact Codex compaction advisory %j", (text) => {
    expect(isCodexCompactionAdvisoryText(text)).toBe(true)
  })

  it.each([
    `${ADVISORY} Please continue.`,
    "Heads up: something else went wrong.",
    "Warning: truncated output",
    "Context compacted to fit the model's context window.",
  ])("keeps non-advisory assistant text %j", (text) => {
    expect(isCodexCompactionAdvisoryText(text)).toBe(false)
  })
})

describe("filterCodexCompactionAdvisoryParts", () => {
  const goalStart: AdaptedToolCallPart = {
    type: "tool-call",
    toolCallId: "goal-1",
    toolName: "update_goal",
    input: null,
    state: "output-available",
  }

  it("filters only assistant display text, including nested goal-run items", () => {
    const parts: AdaptedContentPart[] = [
      { type: "text", text: `Warning: ${ADVISORY}` },
      { type: "text", text: "Continuing after compact." },
      {
        type: "reasoning",
        content: ADVISORY,
        isStreaming: false,
      },
      {
        type: "tool-result",
        toolCallId: "tool-1",
        output: ADVISORY,
        state: "output-available",
      },
      {
        type: "goal-run",
        start: goalStart,
        end: null,
        items: [
          { type: "text", text: ADVISORY },
          { type: "text", text: "nested result stays" },
        ],
        isRunning: false,
      },
    ]

    const filtered = filterCodexCompactionAdvisoryParts(parts)

    expect(filtered).not.toBe(parts)
    expect(filtered).toContainEqual({
      type: "text",
      text: "Continuing after compact.",
    })
    expect(filtered).toContainEqual(
      expect.objectContaining({
        type: "reasoning",
        content: ADVISORY,
      })
    )
    expect(filtered).toContainEqual(
      expect.objectContaining({
        type: "tool-result",
        output: ADVISORY,
      })
    )
    const goal = filtered.find((part) => part.type === "goal-run")
    expect(goal?.type).toBe("goal-run")
    if (goal?.type !== "goal-run") throw new Error("expected goal run")
    expect(goal.items).toEqual([{ type: "text", text: "nested result stays" }])
  })
})
