import { describe, expect, it } from "vitest"

import {
  parseCodexModelConfig,
  serializeCodexModelConfig,
  type CodexModelConfig,
} from "@/lib/types"

describe("Codex structured model config", () => {
  it("round-trips customs + excludedOfficials + default", () => {
    const config: CodexModelConfig = {
      customs: [
        {
          slug: "gw/opus",
          displayName: "Gateway Opus",
          contextWindow: 200000,
          base: "gpt-5.6-sol",
          overrides: { description: "x" },
        },
      ],
      excludedOfficials: ["gpt-5.2"],
      default: "gw/opus",
    }
    const serialized = serializeCodexModelConfig(config)
    expect(serialized).not.toBeNull()
    expect(parseCodexModelConfig(serialized)).toEqual(config)
  })

  // The edit dialog diffs `provider.model !== serialize(state)`; a load→no-edit
  // cycle must reproduce the exact stored string or it reports a spurious change.
  it("is idempotent (serialize∘parse is identity on canonical JSON)", () => {
    const raw = serializeCodexModelConfig({
      customs: [{ slug: "a", base: "gpt-5.4", overrides: { b: 1, a: 2 } }],
      excludedOfficials: ["z", "a"],
      default: "a",
    })!
    expect(serializeCodexModelConfig(parseCodexModelConfig(raw))).toBe(raw)
  })

  it("sorts override keys and excludedOfficials for a byte-stable diff", () => {
    const s = serializeCodexModelConfig({
      customs: [
        { slug: "a", base: "b", overrides: { z: 1, a: 2, m: { y: 1, x: 2 } } },
      ],
      excludedOfficials: ["gpt-b", "gpt-a"],
    })
    expect(s).toBe(
      JSON.stringify({
        customs: [
          {
            slug: "a",
            base: "b",
            overrides: { a: 2, m: { x: 2, y: 1 }, z: 1 },
          },
        ],
        excludedOfficials: ["gpt-a", "gpt-b"],
      })
    )
  })

  it("migrates a legacy {models} catalog to customs", () => {
    expect(
      parseCodexModelConfig(
        JSON.stringify({
          models: [{ slug: "gw/x", base: "gpt-5.4" }],
          default: "gw/x",
        })
      )
    ).toEqual({ customs: [{ slug: "gw/x", base: "gpt-5.4" }], default: "gw/x" })
  })

  it("treats a legacy plain slug as a single custom", () => {
    expect(parseCodexModelConfig("gpt-5.5")).toEqual({
      customs: [{ slug: "gpt-5.5", base: "gpt-5.5" }],
      default: "gpt-5.5",
    })
  })

  it("empty/no-deviation inputs → empty config; serializes to null", () => {
    expect(parseCodexModelConfig(null)).toEqual({ customs: [] })
    expect(parseCodexModelConfig("   ")).toEqual({ customs: [] })
    expect(parseCodexModelConfig('{"customs":[]}')).toEqual({ customs: [] })
    // No customs AND no removed officials → feature off → null.
    expect(serializeCodexModelConfig({ customs: [] })).toBeNull()
    // But removing an official alone is a real deviation → not null.
    expect(
      serializeCodexModelConfig({ customs: [], excludedOfficials: ["gpt-5.2"] })
    ).toBe(JSON.stringify({ customs: [], excludedOfficials: ["gpt-5.2"] }))
  })

  it("preserves a default that names an official (validated later by the backend)", () => {
    const s = serializeCodexModelConfig({
      customs: [{ slug: "a", base: "b" }],
      default: "gpt-5.5",
    })
    expect(s).toBe(
      JSON.stringify({
        customs: [{ slug: "a", base: "b" }],
        default: "gpt-5.5",
      })
    )
  })

  it("skips customs missing a slug and defaults base to the slug", () => {
    const parsed = parseCodexModelConfig(
      JSON.stringify({ customs: [{ base: "b" }, { slug: "keep" }] })
    )
    expect(parsed).toEqual({ customs: [{ slug: "keep", base: "keep" }] })
  })
})
