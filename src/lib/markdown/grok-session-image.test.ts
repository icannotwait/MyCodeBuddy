import { describe, expect, it } from "vitest"

import cases from "../../../fixtures/grok-session-image-href-cases.json"
import {
  GROK_SESSION_IMAGE_MIME_BY_EXTENSION,
  parseGrokSessionImageRef,
} from "./grok-session-image"

describe("parseGrokSessionImageRef", () => {
  it.each(cases.accepted)("accepts $input", ({ input, expected }) => {
    expect(parseGrokSessionImageRef(input)).toEqual(expected)
  })

  it.each(cases.rejected)("rejects $reason: $input", ({ input }) => {
    expect(parseGrokSessionImageRef(input)).toBeNull()
  })

  it("enforces the original-input UTF-8 byte boundary", () => {
    const suffix = "images/a.png"
    expect(
      parseGrokSessionImageRef(`${" ".repeat(1024 - suffix.length)}${suffix}`)
    ).toEqual({ path: suffix, filename: "a.png", extension: "png" })
    expect(
      parseGrokSessionImageRef(`${" ".repeat(1025 - suffix.length)}${suffix}`)
    ).toBeNull()
  })

  it("enforces the decoded filename UTF-8 byte boundary", () => {
    const passName = `${"a".repeat(251)}.png`
    const failName = `${"a".repeat(252)}.png`
    expect(parseGrokSessionImageRef(`images/${passName}`)?.filename).toBe(
      passName
    )
    expect(parseGrokSessionImageRef(`images/${failName}`)).toBeNull()
  })

  it("counts multibyte filename bytes instead of UTF-16 code units", () => {
    const passName = `${"界".repeat(83)}.png`
    const failName = `${"界".repeat(84)}.png`
    expect(new TextEncoder().encode(passName)).toHaveLength(253)
    expect(new TextEncoder().encode(failName)).toHaveLength(256)
    expect(parseGrokSessionImageRef(`images/${passName}`)).not.toBeNull()
    expect(parseGrokSessionImageRef(`images/${failName}`)).toBeNull()
  })

  it("maps every accepted extension to its fixed raster MIME", () => {
    expect(GROK_SESSION_IMAGE_MIME_BY_EXTENSION).toEqual({
      png: "image/png",
      jpg: "image/jpeg",
      jpeg: "image/jpeg",
      webp: "image/webp",
      gif: "image/gif",
    })
  })
})
