import { describe, expect, it } from "vitest"
import type { SessionConfigOptionInfo } from "@/lib/types"
import {
  filterSessionConfigOptions,
  isHiddenSessionConfigOptionId,
} from "./session-config-filter"

function option(id: string): SessionConfigOptionInfo {
  return {
    id,
    name: id,
    kind: {
      type: "select",
      current_value: "a",
      options: [{ value: "a", name: "A" }],
      groups: [],
    },
  }
}

describe("session-config-filter", () => {
  it("identifies the Codex Fast mode config id", () => {
    expect(isHiddenSessionConfigOptionId("fast-mode")).toBe(true)
    expect(isHiddenSessionConfigOptionId("model")).toBe(false)
  })

  it("returns null for null/undefined input", () => {
    expect(filterSessionConfigOptions(null)).toBeNull()
    expect(filterSessionConfigOptions(undefined)).toBeNull()
  })

  it("returns the same array when nothing is hidden", () => {
    const input = [option("model"), option("mode")]
    expect(filterSessionConfigOptions(input)).toBe(input)
  })

  it("strips fast-mode and keeps the rest", () => {
    const input = [option("mode"), option("fast-mode"), option("model")]
    const filtered = filterSessionConfigOptions(input)
    expect(filtered?.map((o) => o.id)).toEqual(["mode", "model"])
  })
})
