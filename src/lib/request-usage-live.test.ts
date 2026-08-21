import { describe, expect, it } from "vitest"
import {
  aliasRequestUsageIds,
  getPublishedRequestUsage,
  publishRequestUsage,
} from "./request-usage-live"
import { EMPTY_REQUEST_USAGE } from "./request-usage-speed"

describe("request-usage-live aliases", () => {
  it("lets a draft runtime id read samples published under the DB id", () => {
    const runtimeId = -9001
    const dbId = 9001
    publishRequestUsage(runtimeId, {
      outputTokens: 40,
      generationMs: 2000,
      tps: 20,
      sampleCount: 1,
      estimatedSampleCount: 0,
    })
    aliasRequestUsageIds(runtimeId, dbId)
    expect(getPublishedRequestUsage(runtimeId).outputTokens).toBe(40)
    expect(getPublishedRequestUsage(dbId).outputTokens).toBe(40)

    publishRequestUsage(dbId, {
      outputTokens: 80,
      generationMs: 4000,
      tps: 20,
      sampleCount: 2,
      estimatedSampleCount: 0,
    })
    expect(getPublishedRequestUsage(runtimeId).sampleCount).toBe(2)
  })

  it("ignores empty ids", () => {
    publishRequestUsage(0, {
      outputTokens: 1,
      generationMs: 1,
      tps: 1000,
      sampleCount: 1,
      estimatedSampleCount: 0,
    })
    expect(getPublishedRequestUsage(0)).toEqual(EMPTY_REQUEST_USAGE)
  })

  it("preserves estimate provenance as part of the aliased whole snapshot", () => {
    const runtimeId = -9101
    const dbId = 9101
    publishRequestUsage(runtimeId, {
      outputTokens: 120,
      generationMs: 2_000,
      tps: 60,
      sampleCount: 2,
      estimatedSampleCount: 1,
    })

    aliasRequestUsageIds(runtimeId, dbId)

    expect(getPublishedRequestUsage(runtimeId).estimatedSampleCount).toBe(1)
    expect(getPublishedRequestUsage(dbId)).toEqual(
      getPublishedRequestUsage(runtimeId)
    )
  })
})
