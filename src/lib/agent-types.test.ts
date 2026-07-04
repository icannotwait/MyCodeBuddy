import { describe, expect, it } from "vitest"

import { AGENT_DISPLAY_ORDER, ALL_AGENT_TYPES, AGENT_LABELS } from "./types"

describe("agent type registry", () => {
  it("does not expose the removed agent", () => {
    const removedType = ["open", "claw"].join("_")
    const removedLabel = ["Open", "Claw"].join("")

    expect(ALL_AGENT_TYPES).not.toContain(removedType)
    expect(AGENT_DISPLAY_ORDER).not.toContain(removedType)
    expect(Object.values(AGENT_LABELS)).not.toContain(removedLabel)
  })
})
