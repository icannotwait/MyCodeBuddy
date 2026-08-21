import { describe, expect, it } from "vitest"
import type { SessionConfigOptionInfo } from "@/lib/types"
import {
  createRequestTokenEstimator,
  discardEstimatedRequest,
  expandVisibleTokens,
  hasPositiveEstimatedOutput,
  hasUnsettledEstimatedRequest,
  observeEstimatedDelta,
  observeEstimatedSnapshot,
  replaceEstimatorFromHydration,
  resolveReasoningProfile,
  settleEstimatedRequest,
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

const mediumCodex = [select("reasoning_effort", "thought_level", "medium")]

function observation(receivedAt: number, configOptions = mediumCodex) {
  return {
    agentType: "codex" as const,
    configOptions,
    receivedAt,
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

describe("request settlement", () => {
  it("starts at first positive output, excludes prior TTFT, and rounds once", () => {
    let state = createRequestTokenEstimator()
    state = observeEstimatedDelta(state, "    ", observation(100))
    expect(hasUnsettledEstimatedRequest(state)).toBe(false)

    state = observeEstimatedDelta(state, "a".repeat(40), observation(500))
    const settled = settleEstimatedRequest(state, 1_500)

    expect(settled.sample).toEqual({
      outputTokens: 17,
      durationMs: 1_000,
      estimated: true,
    })
    expect(hasUnsettledEstimatedRequest(settled.state)).toBe(false)
    expect(settled.state.epoch).toBe(1)
  })

  it("freezes the first positive output profile for the request", () => {
    let state = createRequestTokenEstimator()
    state = observeEstimatedDelta(
      state,
      "a".repeat(40),
      observation(0, mediumCodex)
    )
    state = observeEstimatedDelta(
      state,
      "a".repeat(40),
      observation(500, [select("reasoning_effort", "thought_level", "xhigh")])
    )

    expect(settleEstimatedRequest(state, 1_000).sample?.outputTokens).toBe(33)
  })

  it("discards finite durations below one millisecond and advances the epoch", () => {
    let state = createRequestTokenEstimator()
    state = observeEstimatedDelta(state, "abcd", observation(10))
    const settled = settleEstimatedRequest(state, 10.5)

    expect(settled.sample).toBeNull()
    expect(settled.state.epoch).toBe(1)
    expect(hasUnsettledEstimatedRequest(settled.state)).toBe(false)
  })

  it("ignores a boundary with no active output", () => {
    const state = createRequestTokenEstimator()
    const settled = settleEstimatedRequest(state, 1_000)

    expect(settled.sample).toBeNull()
    expect(settled.state).toBe(state)
  })

  it("discards an active request without changing settled external usage", () => {
    const active = observeEstimatedDelta(
      createRequestTokenEstimator(),
      "abcd",
      observation(10)
    )
    const discarded = discardEstimatedRequest(active)

    expect(discarded.epoch).toBe(1)
    expect(discarded.visibleTokens).toBe(0)
    expect(discarded.startedAt).toBeNull()
  })
})

function settleTenTokenBaseline() {
  let state = createRequestTokenEstimator()
  state = observeEstimatedSnapshot(
    state,
    "tool:1",
    "a".repeat(40),
    observation(0)
  )
  return settleEstimatedRequest(state, 1_000).state
}

describe("epoch-local snapshot accounting", () => {
  it.each([
    ["b".repeat(12), 3],
    ["b".repeat(48), 12],
    ["b".repeat(40), 10],
  ])(
    "counts a first epoch replacement as its full measurement",
    (text, expected) => {
      const next = observeEstimatedSnapshot(
        settleTenTokenBaseline(),
        "tool:1",
        text,
        observation(2_000)
      )

      expect(next.visibleTokens).toBeCloseTo(expected)
    }
  )

  it("does not claim an unchanged inherited snapshot", () => {
    const seeded = settleTenTokenBaseline()
    const unchanged = observeEstimatedSnapshot(
      seeded,
      "tool:1",
      "a".repeat(40),
      observation(2_000)
    )

    expect(unchanged.visibleTokens).toBe(0)
    expect(hasUnsettledEstimatedRequest(unchanged)).toBe(false)
  })

  it("measures only a same-epoch suffix and treats repeats as identity no-ops", () => {
    let state = observeEstimatedSnapshot(
      createRequestTokenEstimator(),
      "plan",
      "a".repeat(40),
      observation(100)
    )
    expect(state.visibleTokens).toBeCloseTo(10)

    state = observeEstimatedSnapshot(
      state,
      "plan",
      "a".repeat(48),
      observation(200)
    )
    expect(state.visibleTokens).toBeCloseTo(12)
    const repeated = observeEstimatedSnapshot(
      state,
      "plan",
      "a".repeat(48),
      observation(300)
    )

    expect(repeated).toBe(state)
    expect(repeated.epoch).toBe(0)
  })

  it("retracts only epoch-local append tokens for 10 to 12 to 3", () => {
    let state = settleTenTokenBaseline()
    state = observeEstimatedSnapshot(
      state,
      "tool:1",
      "a".repeat(48),
      observation(2_000)
    )
    expect(state.visibleTokens).toBeCloseTo(2)

    state = observeEstimatedSnapshot(
      state,
      "tool:1",
      "b".repeat(12),
      observation(2_100)
    )
    expect(state.visibleTokens).toBe(0)
    expect(hasUnsettledEstimatedRequest(state)).toBe(true)
    expect(hasPositiveEstimatedOutput(state)).toBe(false)
  })

  it("reconciles 10 to 12 to 15 to five epoch-local tokens", () => {
    let state = settleTenTokenBaseline()
    state = observeEstimatedSnapshot(
      state,
      "plan",
      "a".repeat(48),
      observation(2_000)
    )
    expect(state.visibleTokens).toBeCloseTo(12)

    state = observeEstimatedSnapshot(
      state,
      "plan",
      "b".repeat(60),
      observation(2_100)
    )
    expect(state.visibleTokens).toBeCloseTo(15)
  })

  it("reconciles the same baseline append then replacement to five", () => {
    let state = settleTenTokenBaseline()
    state = observeEstimatedSnapshot(
      state,
      "tool:1",
      "a".repeat(48),
      observation(2_000)
    )
    state = observeEstimatedSnapshot(
      state,
      "tool:1",
      "b".repeat(60),
      observation(2_100)
    )

    expect(state.visibleTokens).toBeCloseTo(5)
    const baseline = state.baselines.get("tool:1")
    expect(baseline?.snapshotAtEpochStart).toBeCloseTo(10)
    expect(baseline?.epochLocalContribution).toBeCloseTo(5)
  })

  it("seeds hydration baselines without tokens, profile, or clock", () => {
    const hydrated = replaceEstimatorFromHydration(
      observeEstimatedDelta(
        createRequestTokenEstimator(),
        "old",
        observation(10)
      ),
      {
        planText: "hydrated plan",
        toolInputs: [["tool:seeded", "hydrated args"]],
      }
    )

    expect(hydrated.visibleTokens).toBe(0)
    expect(hydrated.startedAt).toBeNull()
    expect(hydrated.frozenProfile).toBeNull()
    expect(hydrated.baselines.get("plan")?.currentText).toBe("hydrated plan")
    expect(hydrated.baselines.get("tool:seeded")?.epochLocalContribution).toBe(
      0
    )

    const unchanged = observeEstimatedSnapshot(
      hydrated,
      "tool:seeded",
      "hydrated args",
      observation(20)
    )
    expect(hasUnsettledEstimatedRequest(unchanged)).toBe(false)
  })
})
