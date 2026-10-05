import { describe, expect, it } from "vitest"

import { roundtableBody, roundtableStatus } from "@/lib/roundtable/api"

describe("roundtable api", () => {
  it("omits principal and clamps the page", () => {
    const body = roundtableBody({ command: "roundtable_list", pageLimit: 100 })
    expect(body).not.toHaveProperty("principal")
    expect(body.page_limit).toBe(100)
    expect(roundtableStatus("capacity_limited")).toBe(429)
    expect(() =>
      roundtableBody({ command: "roundtable_events", pageLimit: 501 })
    ).toThrow()
  })
})
