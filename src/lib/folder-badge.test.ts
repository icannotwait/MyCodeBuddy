import { describe, expect, it } from "vitest"

import { folderBadgeColor, folderBadgeLabel } from "@/lib/folder-badge"

describe("folderBadgeColor", () => {
  it("maps a folder id to a stable palette color", () => {
    expect(folderBadgeColor(0)).toBe("bg-red-500")
    expect(folderBadgeColor(1)).toBe("bg-orange-500")
  })

  it("wraps around the palette (16 colors)", () => {
    expect(folderBadgeColor(16)).toBe(folderBadgeColor(0))
    expect(folderBadgeColor(17)).toBe(folderBadgeColor(1))
  })

  it("is stable for the same id", () => {
    expect(folderBadgeColor(5)).toBe(folderBadgeColor(5))
  })

  it("normalizes negative ids via absolute value", () => {
    expect(folderBadgeColor(-1)).toBe(folderBadgeColor(1))
    expect(folderBadgeColor(-16)).toBe(folderBadgeColor(0))
  })
})

describe("folderBadgeLabel", () => {
  it("returns the uppercased first letter", () => {
    expect(folderBadgeLabel("myproject")).toBe("M")
  })

  it("returns '?' for an empty name", () => {
    expect(folderBadgeLabel("")).toBe("?")
  })

  it("keeps a leading digit as-is", () => {
    expect(folderBadgeLabel("3rd-repo")).toBe("3")
  })

  it("handles unicode letters", () => {
    expect(folderBadgeLabel("é-app")).toBe("É")
    expect(folderBadgeLabel("项目")).toBe("项")
  })

  it("falls back to the first character when it is not alphanumeric", () => {
    // The match is anchored to the first character, not the first
    // alphanumeric anywhere in the string.
    expect(folderBadgeLabel("-dashed")).toBe("-")
    expect(folderBadgeLabel("  spaced")).toBe(" ")
  })
})
