import { describe, expect, it } from "vitest"
import { defaultRehypePlugins } from "streamdown"

import {
  rehypePluginsAllowingCodeg,
  shouldPreserveLocalPathHref,
} from "./rehype-allow-codeg"

/** Pull the href protocol allow-list out of a `[rehypeSanitize, schema]` tuple. */
function hrefProtocols(plugin: unknown): string[] | undefined {
  if (!Array.isArray(plugin)) return undefined
  const schema = plugin[1] as { protocols?: { href?: string[] } } | undefined
  return schema?.protocols?.href
}

type HastNode = {
  type?: string
  tagName?: string
  properties?: { href?: string; title?: string; class?: string }
  children?: HastNode[]
}

function runHardenPlugin(plugin: unknown, href: string): HastNode {
  const factory =
    typeof plugin === "function"
      ? plugin
      : Array.isArray(plugin)
        ? plugin[0]
        : null
  if (typeof factory !== "function") {
    throw new Error("expected rehype plugin factory")
  }
  const transform = factory()
  const tree: HastNode = {
    type: "root",
    children: [
      {
        type: "element",
        tagName: "a",
        properties: { href },
        children: [{ type: "text", value: "x" } as HastNode],
      },
    ],
  }
  transform(tree)
  return tree.children![0]!
}

describe("rehypePluginsAllowingCodeg", () => {
  it("adds `codeg` to the sanitize schema's href protocol allow-list", () => {
    // Guards against an upstream rename of the `sanitize` key — the whole fix
    // hinges on this entry existing.
    const sanitizeIndex = Object.keys(defaultRehypePlugins).indexOf("sanitize")
    expect(sanitizeIndex).toBeGreaterThanOrEqual(0)

    const href = hrefProtocols(
      rehypePluginsAllowingCodeg(defaultRehypePlugins)[sanitizeIndex]
    )
    expect(href).toContain("codeg")
    // Exactly once — no duplicate even if re-derived.
    expect(href?.filter((p) => p === "codeg")).toHaveLength(1)
    // Pre-existing protocols are preserved (https is always present).
    expect(href).toContain("https")
  })

  it("preserves plugin count/order and only rewrites sanitize + harden", () => {
    const keys = Object.keys(defaultRehypePlugins)
    const result = rehypePluginsAllowingCodeg(defaultRehypePlugins)
    expect(result).toHaveLength(keys.length)
    keys.forEach((key, i) => {
      if (key === "sanitize" || key === "harden") return
      expect(result[i]).toBe(defaultRehypePlugins[key])
    })
  })

  it("clones rather than mutating the shipped sanitize schema", () => {
    // The shipped default must not already contain codeg, else the fix is moot.
    expect(hrefProtocols(defaultRehypePlugins.sanitize)).not.toContain("codeg")
    rehypePluginsAllowingCodeg(defaultRehypePlugins)
    // Still absent on the original after deriving — we built a new schema.
    expect(hrefProtocols(defaultRehypePlugins.sanitize)).not.toContain("codeg")
  })

  it.each([
    "docs/a.md",
    "./src/a.ts",
    "../plans/x.md",
    "docs/My%20File.md:12",
    "/repo/src/a.ts",
    "/D:/repo/a.ts",
  ])("preserves local path href through harden: %s", (href) => {
    const keys = Object.keys(defaultRehypePlugins)
    const hardenIndex = keys.indexOf("harden")
    expect(hardenIndex).toBeGreaterThanOrEqual(0)
    const wrapped =
      rehypePluginsAllowingCodeg(defaultRehypePlugins)[hardenIndex]
    const node = runHardenPlugin(wrapped, href)
    expect(node.tagName).toBe("a")
    expect(node.properties?.href).toBe(href)
  })

  it("still hardens non-local hrefs (does not preserve https rewrite)", () => {
    const keys = Object.keys(defaultRehypePlugins)
    const hardenIndex = keys.indexOf("harden")
    const wrapped =
      rehypePluginsAllowingCodeg(defaultRehypePlugins)[hardenIndex]
    // Default streamdown harden allows * prefixes; https survives.
    const node = runHardenPlugin(wrapped, "https://example.com/docs")
    expect(node.tagName).toBe("a")
    expect(node.properties?.href).toMatch(/^https:\/\/example\.com/)
  })
})

describe("shouldPreserveLocalPathHref", () => {
  it.each([
    ["docs/a.md", true],
    ["./src/a.ts", true],
    ["../plans/x.md", true],
    ["docs/My%20File.md:12", true],
    ["/repo/src/a.ts", true],
    ["https://example.com/a", false],
    ["//cdn.example.com/a.js", false],
  ])("%s → %s", (href, expected) => {
    expect(shouldPreserveLocalPathHref(href)).toBe(expected)
  })
})
