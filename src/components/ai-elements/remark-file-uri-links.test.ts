import { describe, expect, it } from "vitest"
import { remarkRewriteFileUriLinks } from "./remark-file-uri-links"

// Minimal mdast node shapes for the transform.
type Node = {
  type: string
  url?: string
  identifier?: string
  children?: Node[]
}

function linkTree(url: string): Node {
  return {
    type: "root",
    children: [
      {
        type: "paragraph",
        children: [{ type: "link", url, children: [{ type: "text" }] }],
      },
    ],
  }
}

function firstLinkUrl(tree: Node): string | undefined {
  let found: string | undefined
  const walk = (n: Node) => {
    if (n.type === "link") found = n.url
    n.children?.forEach(walk)
  }
  walk(tree)
  return found
}

function rewrite(url: string): string | undefined {
  const tree = linkTree(url)
  remarkRewriteFileUriLinks()(tree)
  return firstLinkUrl(tree)
}

describe("remarkRewriteFileUriLinks", () => {
  it("rewrites a POSIX file:// URI to a bare local path", () => {
    expect(rewrite("file:///Users/a/b.ts")).toBe("/Users/a/b.ts")
  })

  it("keeps the harden-safe leading slash before a Windows drive", () => {
    expect(rewrite("file:///C:/x/y.ts")).toBe("/C:/x/y.ts")
  })

  it("preserves encoded path data, query text, and line fragments", () => {
    expect(rewrite("file:///C:/My%20Repo/a%23b.ts?raw=1#L12")).toBe(
      "/C:/My%20Repo/a%23b.ts?raw=1#L12"
    )
  })

  it("keeps an encoded POSIX drive-like segment encoded", () => {
    expect(rewrite("file:///C%3A/repo/app.ts")).toBe("/C%3A/repo/app.ts")
  })

  it("leaves file images and their reference definitions unchanged", () => {
    const tree: Node = {
      type: "root",
      children: [
        {
          type: "paragraph",
          children: [
            { type: "image", url: "file:///C:/image.png" },
            { type: "imageReference", identifier: "img" },
          ],
        },
        {
          type: "definition",
          identifier: "img",
          url: "file:///C:/image.png",
        },
      ],
    }
    const before = structuredClone(tree)
    remarkRewriteFileUriLinks()(tree)
    expect(tree).toEqual(before)
  })

  it("emits a UNC file:// URI as a backslash UNC path (unambiguously local)", () => {
    // //server/share would be indistinguishable from a protocol-relative
    // web url downstream; the backslash form tags it as a local file.
    expect(rewrite("file://server/share/doc.md")).toBe(
      "\\\\server\\share\\doc.md"
    )
  })

  it("preserves fragments on rewritten links", () => {
    expect(rewrite("file:///Users/a/b.ts#L12")).toBe("/Users/a/b.ts#L12")
  })

  it("leaves non-file URLs untouched", () => {
    expect(rewrite("https://example.com/x")).toBe("https://example.com/x")
  })

  // Bare Windows drive hrefs (`D:/…`) are parsed as scheme `D:` by
  // rehype-harden and rendered as "label [blocked]". Prefix a slash so they
  // survive as root-relative local paths (same shape file:// rewrite emits).
  it("prefixes a bare Windows drive href so harden does not block it", () => {
    expect(
      rewrite("D:/MyCodeBuddy/src-tauri/src/acp/delegation/companion.rs:1037")
    ).toBe("/D:/MyCodeBuddy/src-tauri/src/acp/delegation/companion.rs:1037")
  })

  it("normalizes backslashes on bare Windows drive hrefs", () => {
    expect(rewrite(String.raw`D:\repo\src\app.ts`)).toBe("/D:/repo/src/app.ts")
  })

  it("leaves already-safe /D:/ Windows hrefs unchanged", () => {
    expect(rewrite("/D:/repo/src/app.ts:12")).toBe("/D:/repo/src/app.ts:12")
  })
})
