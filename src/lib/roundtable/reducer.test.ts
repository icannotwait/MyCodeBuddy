import { describe, expect, it } from "vitest"

import {
  initialRoundtableView,
  reduceRoundtable,
  resyncRoundtable,
} from "@/lib/roundtable/reducer"
import type { RoundtableEvent } from "@/lib/roundtable/types"

describe("roundtable reducer", () => {
  it("stops at gaps and unknown durable events without advancing the watermark", () => {
    const synced = resyncRoundtable(["snapshot"], 5, "paused")
    const gap = reduceRoundtable(synced, {
      seq: 7,
      kind: "accepted",
      text: "after gap",
    })
    expect(gap.lastSeq).toBe(5)
    expect(gap.catchingUp).toBe(true)
    expect(
      reduceRoundtable(gap, { seq: 6, kind: "accepted", text: "late" }).accepted
    ).toEqual(["snapshot"])
    const unknown = reduceRoundtable(synced, {
      seq: 6,
      kind: "future_schema",
    } as unknown as RoundtableEvent)
    expect(unknown.catchingUp).toBe(true)
    expect(unknown.lastSeq).toBe(5)
  })

  it("does not reopen a closed preview when an old reset arrives", () => {
    let view = reduceRoundtable(initialRoundtableView, {
      seq: 1,
      kind: "preview",
      incarnation: 2,
      attemptId: "a",
      text: "draft",
    })
    view = reduceRoundtable(view, { seq: 1, kind: "accepted", text: "first" })
    view = reduceRoundtable(view, {
      seq: 0,
      kind: "preview_reset",
      incarnation: 3,
      attemptId: "b",
    })
    view = reduceRoundtable(view, {
      seq: 1,
      kind: "preview",
      incarnation: 3,
      attemptId: "b",
      text: "draft b",
    })
    view = reduceRoundtable(view, { seq: 2, kind: "accepted", text: "second" })
    const stale = reduceRoundtable(view, {
      seq: 99,
      kind: "preview_reset",
      incarnation: 2,
      attemptId: "a",
    })
    expect(stale).toBe(view)
  })

  it("replaces accepted business state atomically from a verified projection", () => {
    const synced = resyncRoundtable(["old"], 2)
    const next = reduceRoundtable(synced, {
      seq: 3,
      kind: "projection",
      snapshot: {
        accepted: ["one", "two"],
        highWaterSeq: 3,
        status: "completed",
      },
    })
    expect(next.accepted).toEqual(["one", "two"])
    expect(next.status).toBe("completed")
    expect(next.lastSeq).toBe(3)
  })
  it("restores the durable watermark and ignores snapshot replay", () => {
    const synced = resyncRoundtable(["already accepted"], 8)
    const duplicate = reduceRoundtable(synced, {
      seq: 8,
      kind: "accepted",
      text: "already accepted",
    })
    expect(duplicate.accepted).toEqual(["already accepted"])
    const next = reduceRoundtable(duplicate, {
      seq: 9,
      kind: "accepted",
      text: "next",
    })
    expect(next.accepted).toEqual(["already accepted", "next"])
  })

  it("does not let preview sequences consume durable events", () => {
    const synced = initialRoundtableView
    const preview = reduceRoundtable(synced, {
      seq: 1,
      kind: "preview",
      incarnation: 2,
      text: "draft",
    })
    const accepted = reduceRoundtable(preview, {
      seq: 1,
      kind: "accepted",
      text: "final",
    })
    expect(accepted.accepted).toEqual(["final"])
    const stale = reduceRoundtable(accepted, {
      seq: 9,
      kind: "preview",
      incarnation: 1,
      text: "old process",
    })
    expect(stale.preview).toBeNull()
  })
  it("keeps accepted text stable across preview and resync", () => {
    const preview = reduceRoundtable(initialRoundtableView, {
      seq: 1,
      kind: "preview",
      text: "draft",
    })
    expect(preview.accepted).toEqual([])
    expect(preview.preview).toBe("draft")
    const accepted = reduceRoundtable(preview, {
      seq: 1,
      kind: "accepted",
      text: "final",
    })
    expect(accepted.preview).toBeNull()
    expect(accepted.accepted).toEqual(["final"])
    const duplicate = reduceRoundtable(accepted, {
      seq: 1,
      kind: "accepted",
      text: "other",
    })
    expect(duplicate.accepted).toEqual(["final"])
    const gap = reduceRoundtable(duplicate, { seq: 4, kind: "gap" })
    expect(gap.catchingUp).toBe(true)
    expect(resyncRoundtable(["server"], 4).accepted).toEqual(["server"])
  })
})
