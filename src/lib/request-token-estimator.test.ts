import { describe, expect, it } from "vitest"
import type { SessionConfigOptionInfo } from "@/lib/types"
import {
  expandVisibleTokens,
  resolveReasoningProfile,
} from "./request-token-estimator"

function select(
  id: string,
  category: string | null,
  currentValue: string
): SessionConfigOptionInfo {
  return {
    id,
    name: id,
    category,
    kind: {
      type: "select",
      current_value: currentValue,
      options: [],
      groups: [],
    },
  }
}

describe("resolveReasoningProfile", () => {
  it.each([
    ["xhigh", 0.472],
    ["max", 0.485],
    ["high", 0.41],
    ["medium", 0.4],
    ["low", 0.4],
  ] as const)("uses the GPT %s ratio", (effort, ratio) => {
    expect(
      resolveReasoningProfile("codex", [
        select(" Reasoning_Effort ", " Thought_Level ", ` ${effort} `),
      ])
    ).toEqual({ provider: "gpt", effort, reasoningRatio: ratio })
  })

  it.each([
    ["xhigh", 0.556],
    ["high", 0.63],
    ["medium", 0.4],
    ["low", 0.4],
  ] as const)("uses the Grok %s ratio", (effort, ratio) => {
    expect(
      resolveReasoningProfile("grok", [
        select("reasoning_effort", "mode", effort),
      ])
    ).toEqual({ provider: "grok", effort, reasoningRatio: ratio })
  })

  it("prefers the provider-specific selector over model brackets", () => {
    expect(
      resolveReasoningProfile("codex", [
        select("model", "model", "gpt-5[high]"),
        select("reasoning_effort", "thought_level", "low"),
      ])
    ).toMatchObject({ effort: "low", reasoningRatio: 0.4 })
  })

  it.each([
    ["gpt-5[high]", "high"],
    ["gpt-5[reasoning_effort=medium,mode=plan]", "medium"],
    ["gpt-5[effort=low,mode=plan]", "low"],
    ["gpt-5[effort=low,reasoning_effort=high]", "high"],
  ] as const)("parses one trailing model payload %s", (value, effort) => {
    expect(
      resolveReasoningProfile("codex", [select("model", null, value)])?.effort
    ).toBe(effort)
  })

  it.each([
    "gpt-5[]",
    "gpt-5[[high]]",
    "gpt-5[high][low]",
    "gpt-5[mode=plan]",
    "gpt-5[reasoning_effort=high",
    "gpt-5[ultra]",
  ])("falls back for malformed or unsupported model value %s", (value) => {
    expect(
      resolveReasoningProfile("codex", [select("model", "model", value)])
    ).toEqual({
      provider: "gpt",
      effort: "unknown",
      reasoningRatio: 0.467,
    })
  })

  it("does not confuse generic mode or labels with Codex effort", () => {
    expect(
      resolveReasoningProfile("codex", [
        select("mode", "mode", "high"),
        select("approval", "thought_level", "high"),
      ])
    ).toMatchObject({ effort: "unknown", reasoningRatio: 0.467 })
  })

  it("uses Grok unknown fallback and rejects unsupported max", () => {
    expect(
      resolveReasoningProfile("grok", [
        select("reasoning_effort", "mode", "max"),
      ])
    ).toEqual({
      provider: "grok",
      effort: "unknown",
      reasoningRatio: 0.57,
    })
  })

  it("does not opt other agents into estimation", () => {
    expect(resolveReasoningProfile("claude_code", [])).toBeNull()
  })
})

describe("expandVisibleTokens", () => {
  it("uses visible / (1 - q) and rounds once", () => {
    expect(expandVisibleTokens(60, 0.4)).toBe(100)
  })

  it.each([
    [0, 0.4],
    [-1, 0.4],
    [10, -0.1],
    [10, 1],
    [Number.POSITIVE_INFINITY, 0.4],
    [10, Number.NaN],
  ])("rejects invalid visible=%s ratio=%s", (visible, ratio) => {
    expect(expandVisibleTokens(visible, ratio)).toBeNull()
  })
})
