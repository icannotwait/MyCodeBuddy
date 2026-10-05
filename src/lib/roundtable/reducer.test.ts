import { describe, expect, it } from "vitest"

import {
  initialRoundtableView,
  reduceRoundtable,
  resyncRoundtable,
} from "@/lib/roundtable/reducer"

describe("roundtable reducer", () => {
  it("keeps accepted text stable across preview and resync", () => {
    const seen = new Set<number>()
    const preview = reduceRoundtable(
      initialRoundtableView,
      { seq: 1, kind: "preview", text: "draft" },
      seen
    )
    expect(preview.accepted).toEqual([])
    expect(preview.preview).toBe("draft")
    const accepted = reduceRoundtable(
      preview,
      { seq: 2, kind: "accepted", text: "final" },
      seen
    )
    expect(accepted.preview).toBeNull()
    expect(accepted.accepted).toEqual(["final"])
    const duplicate = reduceRoundtable(
      accepted,
      { seq: 2, kind: "accepted", text: "other" },
      seen
    )
    expect(duplicate.accepted).toEqual(["final"])
    const gap = reduceRoundtable(duplicate, { seq: 4, kind: "gap" }, seen)
    expect(gap.catchingUp).toBe(true)
    expect(resyncRoundtable(["server"]).accepted).toEqual(["server"])
  })
})
