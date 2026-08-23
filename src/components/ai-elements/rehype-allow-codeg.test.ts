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
  properties?: {
    href?: string
    src?: string
    alt?: string
    title?: string
    class?: string
    [key: string]: unknown
  }
  children?: HastNode[]
}

function hardenEntry(plugins: unknown[]): unknown {
  const hardenIndex = Object.keys(defaultRehypePlugins).indexOf("harden")
  expect(hardenIndex).toBeGreaterThanOrEqual(0)
  return plugins[hardenIndex]
}

function runHardenTree(plugin: unknown, children: HastNode[]): HastNode {
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
    children,
  }
  transform(tree)
  return tree
}

function runHardenElement(plugin: unknown, element: HastNode): HastNode {
  return runHardenTree(plugin, [
    {
      type: "element",
      children: [],
      ...element,
    },
  ]).children![0]!
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
    const wrapped = hardenEntry(
      rehypePluginsAllowingCodeg(defaultRehypePlugins)
    )
    const node = runHardenElement(wrapped, {
      tagName: "a",
      properties: { href },
      children: [{ type: "text", value: "x" } as HastNode],
    })
    expect(node.tagName).toBe("a")
    expect(node.properties?.href).toBe(href)
  })

  it("still hardens non-local hrefs (does not preserve https rewrite)", () => {
    const wrapped = hardenEntry(
      rehypePluginsAllowingCodeg(defaultRehypePlugins)
    )
    // Default streamdown harden allows * prefixes; https survives.
    const node = runHardenElement(wrapped, {
      tagName: "a",
      properties: { href: "https://example.com/docs" },
      children: [{ type: "text", value: "x" } as HastNode],
    })
    expect(node.tagName).toBe("a")
    expect(node.properties?.href).toMatch(/^https:\/\/example\.com/)
  })

  it("default options still let harden block a local image", () => {
    const harden = hardenEntry(rehypePluginsAllowingCodeg(defaultRehypePlugins))
    const node = runHardenElement(harden, {
      tagName: "img",
      properties: { src: "images/2.png", alt: "x" },
    })
    expect(node.tagName).not.toBe("codeg-grok-session-image")
    expect(JSON.stringify(node)).toContain("blocked")
  })

  it("opt-in preserves and retags exactly a valid Grok image", () => {
    const harden = hardenEntry(
      rehypePluginsAllowingCodeg(defaultRehypePlugins, {
        grokSessionImages: true,
      })
    )
    const node = runHardenElement(harden, {
      tagName: "img",
      properties: {
        src: "images/2.png",
        alt: "x",
        title: "drop",
        onerror: "drop",
        "data-model": "drop",
      },
    })
    expect(node).toMatchObject({
      tagName: "codeg-grok-session-image",
      properties: { src: "images/2.png", alt: "x" },
    })
    expect(Object.keys(node.properties ?? {}).sort()).toEqual(["alt", "src"])
  })

  it.each([
    "docs/foo.png",
    "images/a/b.png",
    "file:///tmp/a.png",
    "images/a.svg",
    "images/%ZZ.png",
  ])("does not preserve a non-matching image: %s", (src) => {
    const harden = hardenEntry(
      rehypePluginsAllowingCodeg(defaultRehypePlugins, {
        grokSessionImages: true,
      })
    )
    const node = runHardenElement(harden, {
      tagName: "img",
      properties: { src, alt: "x" },
    })
    expect(node.tagName).not.toBe("codeg-grok-session-image")
  })

  it("keeps https images on the ordinary img tag under opt-in", () => {
    const harden = hardenEntry(
      rehypePluginsAllowingCodeg(defaultRehypePlugins, {
        grokSessionImages: true,
      })
    )
    const node = runHardenElement(harden, {
      tagName: "img",
      properties: { src: "https://example.com/a.png", alt: "remote" },
    })
    expect(node.tagName).toBe("img")
    expect(node.properties?.src).toBe("https://example.com/a.png")
  })

  it("restores a local anchor and valid image independently in one tree", () => {
    const harden = hardenEntry(
      rehypePluginsAllowingCodeg(defaultRehypePlugins, {
        grokSessionImages: true,
      })
    )
    const tree = runHardenTree(harden, [
      {
        type: "element",
        tagName: "a",
        properties: { href: "docs/a.md" },
        children: [{ type: "text", value: "doc" } as HastNode],
      },
      {
        type: "element",
        tagName: "img",
        properties: { src: "images/2.png", alt: "x" },
        children: [],
      },
    ])

    expect(tree.children?.[0]).toMatchObject({
      tagName: "a",
      properties: { href: "docs/a.md" },
    })
    expect(tree.children?.[1]).toMatchObject({
      tagName: "codeg-grok-session-image",
      properties: { src: "images/2.png", alt: "x" },
    })
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
