import { describe, expect, it } from "vitest"
import {
  resolveSessionModelDisplay,
  SESSION_MODEL_CONFIG_ID,
  SESSION_REASONING_CONFIG_ID,
} from "@/lib/status-bar-session-model"
import type { SessionConfigOptionInfo } from "@/lib/types"

function selectOption(
  id: string,
  current: string,
  options: Array<{ value: string; name: string }>,
  category?: string
): SessionConfigOptionInfo {
  return {
    id,
    name: id,
    category: category ?? null,
    kind: {
      type: "select",
      current_value: current,
      options: options.map((o) => ({
        value: o.value,
        name: o.name,
        description: null,
      })),
      groups: [],
    },
  }
}

describe("resolveSessionModelDisplay", () => {
  it("uses turn archive model and effort when present", () => {
    const display = resolveSessionModelDisplay({
      configOptions: [
        selectOption(
          SESSION_MODEL_CONFIG_ID,
          "gpt-5.1",
          [{ value: "gpt-5.1", name: "GPT-5.1" }],
          "model"
        ),
        selectOption(
          SESSION_REASONING_CONFIG_ID,
          "low",
          [{ value: "low", name: "Low" }],
          "thought_level"
        ),
      ],
      conversationModel: "gpt-5.6-sol",
      conversationEffort: "max",
    })
    expect(display).toEqual({
      model: "gpt-5.6-sol",
      thinkingLevel: "max",
    })
  })

  it("does not show effort from live config when history has none", () => {
    const display = resolveSessionModelDisplay({
      configOptions: [
        selectOption(
          SESSION_REASONING_CONFIG_ID,
          "high",
          [{ value: "high", name: "High" }],
          "thought_level"
        ),
      ],
      conversationModel: "gpt-5.1-codex",
      conversationEffort: null,
    })
    expect(display).toEqual({
      model: "gpt-5.1-codex",
      thinkingLevel: null,
    })
  })

  it("returns nulls when nothing is available", () => {
    expect(
      resolveSessionModelDisplay({
        configOptions: null,
        conversationModel: null,
        conversationEffort: null,
      })
    ).toEqual({
      model: null,
      thinkingLevel: null,
    })
  })
})
