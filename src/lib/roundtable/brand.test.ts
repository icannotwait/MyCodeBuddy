import { describe, expect, it } from "vitest"
import {
  NEUTRAL_BRAND,
  ROUNDTABLE_BRANDS,
  roundtableBrand,
  roundtableBrandKey,
  roundtableSeatBrands,
  shadeRoundtableBrand,
} from "@/lib/roundtable/brand"
import { AGENT_COLORS, type BuiltinAgentType } from "@/lib/types"

function luminance(hex: string) {
  const [r, g, b] = [1, 3, 5].map((index) => {
    const channel = parseInt(hex.slice(index, index + 2), 16) / 255
    return channel <= 0.03928
      ? channel / 12.92
      : ((channel + 0.055) / 1.055) ** 2.4
  })
  return 0.2126 * r + 0.7152 * g + 0.0722 * b
}

function contrast(left: string, right: string) {
  const [high, low] = [luminance(left), luminance(right)].sort((a, b) => b - a)
  return (high + 0.05) / (low + 0.05)
}

describe("roundtable brand colors", () => {
  it("covers every built-in agent with a brand", () => {
    for (const agent of Object.keys(AGENT_COLORS) as BuiltinAgentType[]) {
      expect(roundtableBrandKey(agent), agent).toBe(agent)
    }
  })

  it("uses the requested brand hues", () => {
    expect(roundtableBrand("grok").color).toBe("#111111")
    expect(roundtableBrand("grok").colorDark).toBe("#E5E5E5")
    expect(roundtableBrand("claude_code").color).toBe("#D97757")
    expect(roundtableBrand("antigravity").color).toBe("#1A73E8")
    expect(roundtableBrand("gemini").color).toBe("#3186FF")
    expect(roundtableBrand("codex").color).toBe("#7A9DFF")
  })

  it("keeps name text readable in light and dark mode", () => {
    for (const brand of [...Object.values(ROUNDTABLE_BRANDS), NEUTRAL_BRAND]) {
      expect(contrast(brand.text, "#ffffff"), brand.label).toBeGreaterThan(4.5)
      expect(contrast(brand.text, "#f5f5f5"), brand.label).toBeGreaterThan(4.5)
      expect(contrast(brand.textDark, "#0a0a0a"), brand.label).toBeGreaterThan(
        4.5
      )
      expect(contrast(brand.textDark, "#262626"), brand.label).toBeGreaterThan(
        4.5
      )
    }
  })

  it("falls back from aliases and model hints to a neutral brand", () => {
    expect(roundtableBrandKey("Claude")).toBe("claude_code")
    expect(roundtableBrandKey("openai")).toBe("codex")
    expect(roundtableBrandKey("custom:my-agent", ["claude-sonnet-5"])).toBe(
      "claude_code"
    )
    expect(roundtableBrandKey("custom:x", ["gpt-5.2"])).toBe("codex")
    expect(roundtableBrandKey(undefined, ["qwen3-coder"])).toBe("qwen")
    expect(roundtableBrandKey("custom:x", ["provider:local"])).toBe("neutral")
    expect(roundtableBrandKey(null)).toBe("neutral")
    expect(roundtableBrand("neutral")).toBe(NEUTRAL_BRAND)
  })

  it("keys seats by agent, not seat, and shades members sharing an agent", () => {
    const seats = roundtableSeatBrands({
      participants: [
        { ordinal: 2, role: "c", provider_ref: "p", agent: "grok" },
        { ordinal: 0, role: "a", provider_ref: "p", agent: "grok" },
        { ordinal: 1, role: "b", provider_ref: "p", agent: "antigravity" },
        { ordinal: 3, role: "d", provider_ref: "p" },
      ],
    })
    expect(seats.get(0)).toMatchObject({
      key: "grok",
      variant: 0,
      shared: true,
    })
    expect(seats.get(2)).toMatchObject({
      key: "grok",
      variant: 1,
      shared: true,
    })
    expect(seats.get(1)).toMatchObject({
      key: "antigravity",
      variant: 0,
      shared: false,
    })
    // An absent agent means Codex.
    expect(seats.get(3)?.key).toBe("codex")
    const first = seats.get(0)!.brand
    const second = seats.get(2)!.brand
    expect(second.color).not.toBe(first.color)
    expect(second.colorDark).not.toBe(first.colorDark)
    // Text shades are kept so contrast does not drop.
    expect(second.text).toBe(first.text)
    expect(second.textDark).toBe(first.textDark)
  })

  it("shades toward the page background and stays within the hue", () => {
    const shaded = shadeRoundtableBrand(roundtableBrand("claude_code"), 1)
    expect(shaded.color).toBe("#E4A089")
    expect(luminance(shaded.color)).toBeGreaterThan(luminance("#D97757"))
    expect(luminance(shaded.colorDark)).toBeLessThan(luminance("#E0896B"))
  })
})
