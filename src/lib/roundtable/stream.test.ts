import { describe, expect, it } from "vitest"

import { coalescePreview, resyncAfterReconnect } from "@/lib/roundtable/stream"

describe("roundtable stream", () => {
  it("coalesces the open window and drops an old incarnation", () => {
    const window = coalescePreview(
      [
        { incarnation: 2, seq: 1 },
        { incarnation: 1, seq: 9 },
        { incarnation: 2, seq: 4 },
      ],
      2
    )
    expect(window).toEqual({ first: 1, last: 4 })
    expect(resyncAfterReconnect(["durable"]).preview).toBeNull()
  })
})
