import { describe, expect, it } from "vitest"
import {
  RequestUsageAccumulator,
  accumulateRequestUsage,
  EMPTY_REQUEST_USAGE,
  hiddenUserTurnsFromDetail,
  overlayGenerationOnTurns,
  resolveRequestUsageSample,
  supportsRequestUsageDisplay,
  userOrdinalForCurrentTurn,
  type OverlayTurn,
  type RequestUsageSample,
} from "./request-usage-speed"

describe("supportsRequestUsageDisplay", () => {
  it("is on for Claude, Codex, and Grok", () => {
    expect(supportsRequestUsageDisplay("claude_code")).toBe(true)
    expect(supportsRequestUsageDisplay("codex")).toBe(true)
    expect(supportsRequestUsageDisplay("grok")).toBe(true)
  })

  it("is off for agents without a request-usage adapter", () => {
    expect(supportsRequestUsageDisplay("cursor")).toBe(false)
    expect(supportsRequestUsageDisplay("gemini")).toBe(false)
    expect(supportsRequestUsageDisplay("custom:foo")).toBe(false)
  })
})

describe("RequestUsageAccumulator", () => {
  it("starts at zero with no samples", () => {
    const acc = new RequestUsageAccumulator()
    expect(acc.snapshot()).toEqual({
      outputTokens: 0,
      generationMs: 0,
      tps: 0,
      sampleCount: 0,
      estimatedSampleCount: 0,
    })
  })

  it("weights tps by tokens over generation time", () => {
    const acc = new RequestUsageAccumulator()
    acc.push({ outputTokens: 100, durationMs: 1000 })
    acc.push({ outputTokens: 300, durationMs: 1000 })
    const snap = acc.snapshot()
    expect(snap.outputTokens).toBe(400)
    expect(snap.generationMs).toBe(2000)
    expect(snap.tps).toBeCloseTo(200)
    expect(snap.sampleCount).toBe(2)
  })

  it("ignores a zero-token sample", () => {
    const acc = new RequestUsageAccumulator()
    acc.push({ outputTokens: 0, durationMs: 5000 })
    expect(acc.snapshot().sampleCount).toBe(0)
  })

  it("skips a sample with no usable duration", () => {
    const acc = new RequestUsageAccumulator()
    acc.push({ outputTokens: 50, durationMs: 0 })
    acc.push({ outputTokens: 50, durationMs: undefined })
    expect(acc.snapshot()).toEqual({
      outputTokens: 0,
      generationMs: 0,
      tps: 0,
      sampleCount: 0,
      estimatedSampleCount: 0,
    })
  })

  it("resets to zero", () => {
    const acc = new RequestUsageAccumulator()
    acc.push({
      outputTokens: 10,
      durationMs: 1000,
    } satisfies RequestUsageSample)
    acc.reset()
    expect(acc.snapshot().sampleCount).toBe(0)
    expect(acc.snapshot().tps).toBe(0)
  })

  it("uses a measured clock when the sample has no duration", () => {
    const acc = new RequestUsageAccumulator()
    acc.push(resolveRequestUsageSample({ outputTokens: 80 }, 2000))
    expect(acc.snapshot()).toMatchObject({
      outputTokens: 80,
      generationMs: 2000,
      sampleCount: 1,
    })
    expect(acc.snapshot().tps).toBeCloseTo(40)
  })
})

describe("estimated request usage provenance", () => {
  it("keeps all-exact aggregates exact", () => {
    const exact = accumulateRequestUsage(EMPTY_REQUEST_USAGE, {
      outputTokens: 100,
      durationMs: 1_000,
    })

    expect(exact).toEqual({
      outputTokens: 100,
      generationMs: 1_000,
      tps: 100,
      sampleCount: 1,
      estimatedSampleCount: 0,
    })
  })

  it("counts estimated samples without losing token-weighted rate", () => {
    const estimated = accumulateRequestUsage(EMPTY_REQUEST_USAGE, {
      outputTokens: 100,
      durationMs: 1_000,
      estimated: true,
    })
    const mixed = accumulateRequestUsage(estimated, {
      outputTokens: 300,
      durationMs: 1_000,
    })

    expect(mixed).toEqual({
      outputTokens: 400,
      generationMs: 2_000,
      tps: 200,
      sampleCount: 2,
      estimatedSampleCount: 1,
    })
  })

  it("tracks provenance in RequestUsageAccumulator", () => {
    const acc = new RequestUsageAccumulator()
    acc.push({ outputTokens: 20, durationMs: 500, estimated: true })
    acc.push({ outputTokens: 30, durationMs: 500 })

    expect(acc.snapshot()).toMatchObject({
      sampleCount: 2,
      estimatedSampleCount: 1,
      outputTokens: 50,
      generationMs: 1_000,
    })
  })
})

describe("userOrdinalForCurrentTurn", () => {
  it("uses the last user turn in a fully loaded history", () => {
    expect(
      userOrdinalForCurrentTurn({
        totalUserTurnCount: 3,
        returnedUserTurnCount: 3,
        loadedTurns: [
          { id: "u0", role: "user" },
          { id: "a0", role: "assistant" },
          { id: "u1", role: "user" },
          { id: "a1", role: "assistant" },
          { id: "u2", role: "user" },
        ],
        localTurns: [],
      })
    ).toBe(2)
  })

  it("counts a not-yet-parsed optimistic user turn after a windowed tail", () => {
    expect(
      userOrdinalForCurrentTurn({
        totalUserTurnCount: 20,
        returnedUserTurnCount: 5,
        loadedTurns: [
          { id: "u15", role: "user" },
          { id: "a15", role: "assistant" },
        ],
        localTurns: [{ id: "opt-u", role: "user" }],
      })
    ).toBe(16)
  })

  it("does not double-count a flushed user turn that is also local", () => {
    expect(
      userOrdinalForCurrentTurn({
        totalUserTurnCount: 2,
        returnedUserTurnCount: 2,
        loadedTurns: [
          { id: "u0", role: "user" },
          { id: "a0", role: "assistant" },
          { id: "u1", role: "user" },
        ],
        localTurns: [{ id: "u1", role: "user" }],
      })
    ).toBe(1)
  })
})

describe("overlayGenerationOnTurns", () => {
  it("stamps the first assistant after each matching user ordinal", () => {
    const turns = overlayGenerationOnTurns<OverlayTurn>(
      [
        { id: "u0", role: "user" },
        { id: "a0", role: "assistant" },
        { id: "u1", role: "user" },
        { id: "a1", role: "assistant" },
        { id: "a1b", role: "assistant" },
      ],
      [{ userOrdinal: 1, generationMs: 2500, generationTokens: 400 }]
    )
    expect(turns[1].generation_ms).toBeUndefined()
    expect(turns[3]).toMatchObject({
      id: "a1",
      generation_ms: 2500,
      generation_tokens: 400,
    })
    expect(turns[4].generation_ms).toBeUndefined()
  })

  it("does not attach a prior user's stat across a following user", () => {
    const turns = overlayGenerationOnTurns<OverlayTurn>(
      [
        { id: "u0", role: "user" },
        { id: "u1", role: "user" },
        { id: "a1", role: "assistant" },
      ],
      [{ userOrdinal: 0, generationMs: 900, generationTokens: 10 }]
    )
    expect(turns[2].generation_ms).toBeUndefined()
  })

  it("applies a window offset so a tail page maps global ordinals", () => {
    const turns = overlayGenerationOnTurns<OverlayTurn>(
      [
        { id: "u15", role: "user" },
        { id: "a15", role: "assistant" },
      ],
      [{ userOrdinal: 15, generationMs: 100, generationTokens: 20 }],
      15
    )
    expect(turns[1]).toMatchObject({
      generation_ms: 100,
      generation_tokens: 20,
    })
  })

  it("leaves turns unchanged when there are no stats", () => {
    const input = [{ id: "u0", role: "user" as const }]
    expect(overlayGenerationOnTurns(input, [])).toBe(input)
  })
})

describe("hiddenUserTurnsFromDetail", () => {
  it("uses history_window totals when present", () => {
    expect(
      hiddenUserTurnsFromDetail({
        history_window: {
          total_user_turn_count: 20,
          returned_user_turn_count: 5,
        },
        user_turns_before_offset: 99,
      })
    ).toBe(15)
  })

  it("falls back to the index-window prefix user count", () => {
    expect(
      hiddenUserTurnsFromDetail({
        user_turns_before_offset: 12,
      })
    ).toBe(12)
  })
})
