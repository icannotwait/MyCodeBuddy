import { describe, expect, it } from "vitest"
import type { DbConversationSummary } from "@/lib/types"
import {
  getEffectiveConversationUpdatedAt,
  nextOptimisticActivityTimestamp,
  parseActivityTimestamp,
} from "./conversation-activity"

const summary = {
  id: 7,
  updated_at: "2026-07-18T01:00:00.000Z",
} as DbConversationSummary

describe("conversation activity timestamps", () => {
  it("parses invalid timestamps as zero", () => {
    expect(parseActivityTimestamp("bad")).toBe(0)
    expect(parseActivityTimestamp(null)).toBe(0)
  })

  it("uses the later optimistic timestamp for presentation", () => {
    const optimistic = new Map([
      [
        7,
        {
          token: "t1",
          baselineUpdatedAt: summary.updated_at,
          effectiveAt: "2026-07-18T02:00:00.000Z",
        },
      ],
    ])
    expect(getEffectiveConversationUpdatedAt(summary, optimistic)).toBe(
      "2026-07-18T02:00:00.000Z"
    )
  })

  it("keeps two same-millisecond dispatches strictly monotonic", () => {
    const first = nextOptimisticActivityTimestamp(
      summary.updated_at,
      0,
      1_752_800_000_000
    )
    const second = nextOptimisticActivityTimestamp(
      summary.updated_at,
      first.effectiveMs,
      1_752_800_000_000
    )
    expect(second.effectiveMs).toBe(first.effectiveMs + 1)
  })
})
