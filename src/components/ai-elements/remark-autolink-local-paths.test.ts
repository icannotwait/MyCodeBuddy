import { describe, expect, it } from "vitest"
import { remarkAutolinkLocalPaths } from "./remark-autolink-local-paths"

type Node = {
  type: string
  value?: string
  url?: string
  children?: Node[]
  position?: {
    start: { offset?: number }
    end: { offset?: number }
  }
}

function paragraph(value: string): Node {
  return {
    type: "root",
    children: [
      {
        type: "paragraph",
        children: [{ type: "text", value }],
      },
    ],
  }
}

function visibleText(node: Node): string {
  if (typeof node.value === "string") return node.value
  return node.children?.map(visibleText).join("") ?? ""
}

describe("remarkAutolinkLocalPaths", () => {
  it("splits prose into text, link, and text without changing visible text", () => {
    const source = String.raw`changed D:\repo\src\app.ts now`
    const tree = paragraph(source)
    tree.children![0].children![0].position = {
      start: { offset: 0 },
      end: { offset: source.length },
    }
    remarkAutolinkLocalPaths()(tree, { value: source })
    expect(tree.children?.[0].children).toEqual([
      { type: "text", value: "changed " },
      {
        type: "link",
        url: "/D:/repo/src/app.ts",
        children: [
          { type: "text", value: String.raw`D:\repo\src\app.ts` },
        ],
      },
      { type: "text", value: " now" },
    ])
    expect(visibleText(tree)).toBe(source)
  })

  it("keeps outer quotes outside a path-with-spaces link", () => {
    const tree = paragraph(String.raw`see "D:\My Project\app.ts" now`)
    remarkAutolinkLocalPaths()(tree)
    expect(tree.children?.[0].children).toEqual([
      { type: "text", value: 'see "' },
      {
        type: "link",
        url: "/D:/My%20Project/app.ts",
        children: [
          { type: "text", value: String.raw`D:\My Project\app.ts` },
        ],
      },
      { type: "text", value: '" now' },
    ])
  })

  it("creates multiple non-overlapping links", () => {
    const tree = paragraph(
      String.raw`D:\repo\a.ts and /Users/me/repo/b.ts#L4`
    )
    remarkAutolinkLocalPaths()(tree)
    const links = tree.children?.[0].children?.filter(
      (child) => child.type === "link"
    )
    expect(links?.map((link) => link.url)).toEqual([
      "/D:/repo/a.ts",
      "/Users/me/repo/b.ts#L4",
    ])
  })

  it("does not descend into links, link references, code, or html", () => {
    const path = String.raw`D:\repo\src\app.ts`
    const tree: Node = {
      type: "root",
      children: [
        {
          type: "paragraph",
          children: [
            {
              type: "link",
              url: "https://example.com",
              children: [{ type: "text", value: path }],
            },
            {
              type: "linkReference",
              children: [{ type: "text", value: path }],
            },
            { type: "inlineCode", value: path },
            { type: "html", value: `<span>${path}</span>` },
            { type: "image", url: path },
            { type: "imageReference" },
          ],
        },
        { type: "code", value: path },
        { type: "definition", url: path },
      ],
    }
    const before = structuredClone(tree)
    remarkAutolinkLocalPaths()(tree)
    expect(tree).toEqual(before)
  })

  it("leaves a detected range as text when href encoding fails", () => {
    const source = "/tmp/\uD800.ts"
    const tree = paragraph(source)
    remarkAutolinkLocalPaths()(tree)
    expect(tree).toEqual(paragraph(source))
    expect(visibleText(tree)).toBe(source)
  })

  it("skips a text node changed by CommonMark backslash escaping", () => {
    const source = String.raw`D:\repo\[draft]\app.ts`
    const parsed = String.raw`D:\repo[draft]\app.ts`
    const tree = paragraph(parsed)
    const textNode = tree.children![0].children![0]
    textNode.position = {
      start: { offset: 0 },
      end: { offset: source.length },
    }
    const before = structuredClone(tree)

    remarkAutolinkLocalPaths()(tree, { value: source })

    expect(tree).toEqual(before)
    expect(visibleText(tree)).toBe(parsed)
  })

  it("is idempotent", () => {
    const tree = paragraph("see /Users/me/repo/app.ts")
    const transform = remarkAutolinkLocalPaths()
    transform(tree)
    const once = structuredClone(tree)
    transform(tree)
    expect(tree).toEqual(once)
  })
})
