import { describe, expect, it } from "vitest"

import { configOptionDisplayLabel } from "./session-config-display"

describe("configOptionDisplayLabel", () => {
  it("prefers the compound wire value over a short base name (Cursor)", () => {
    const compound =
      "claude-opus-4-6[thinking=true,context=200k,effort=high,fast=false]"
    expect(
      configOptionDisplayLabel({
        value: compound,
        name: "claude-opus-4-6",
      })
    ).toBe(compound)
  })

  it("keeps a distinct human name when value is not an extension of name", () => {
    expect(
      configOptionDisplayLabel({
        value: "opencode/big-pickle",
        name: "Big Pickle",
      })
    ).toBe("Big Pickle")
  })

  it("returns the shared text when name and value match", () => {
    expect(
      configOptionDisplayLabel({ value: "agent", name: "agent" })
    ).toBe("agent")
  })

  it("falls back to the other side when one side is empty", () => {
    expect(configOptionDisplayLabel({ value: "id", name: "" })).toBe("id")
    expect(configOptionDisplayLabel({ value: "", name: "Label" })).toBe(
      "Label"
    )
  })
})
