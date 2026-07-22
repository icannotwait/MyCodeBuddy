import { describe, expect, it } from "vitest"

import { shouldCloseTabOnEscape } from "./should-close-tab-on-escape"

function escapeEvent(
  overrides: Partial<{
    key: string
    defaultPrevented: boolean
    metaKey: boolean
    ctrlKey: boolean
    altKey: boolean
    shiftKey: boolean
  }> = {}
) {
  return {
    key: "Escape",
    defaultPrevented: false,
    metaKey: false,
    ctrlKey: false,
    altKey: false,
    shiftKey: false,
    ...overrides,
  }
}

describe("shouldCloseTabOnEscape", () => {
  it("accepts bare Escape", () => {
    expect(shouldCloseTabOnEscape(escapeEvent())).toBe(true)
  })

  it('accepts legacy "Esc" key', () => {
    expect(shouldCloseTabOnEscape(escapeEvent({ key: "Esc" }))).toBe(true)
  })

  it("rejects when defaultPrevented", () => {
    expect(
      shouldCloseTabOnEscape(escapeEvent({ defaultPrevented: true }))
    ).toBe(false)
  })

  it("rejects Escape with modifiers", () => {
    expect(shouldCloseTabOnEscape(escapeEvent({ metaKey: true }))).toBe(false)
    expect(shouldCloseTabOnEscape(escapeEvent({ ctrlKey: true }))).toBe(false)
    expect(shouldCloseTabOnEscape(escapeEvent({ altKey: true }))).toBe(false)
    expect(shouldCloseTabOnEscape(escapeEvent({ shiftKey: true }))).toBe(false)
  })

  it("rejects non-Escape keys", () => {
    expect(shouldCloseTabOnEscape(escapeEvent({ key: "Enter" }))).toBe(false)
    expect(shouldCloseTabOnEscape(escapeEvent({ key: "w" }))).toBe(false)
  })
})
