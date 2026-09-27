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

  it("leaves a bare relative path to the rehype step (not a drive path)", () => {
    // `C:` needs a following slash to be a drive path. A relative path gets
    // past sanitize as it is; the `./` it needs is added after sanitize, in
    // rehype-relative-file-links, where raw HTML anchors are covered too.
    expect(rewrite("src/main.rs")).toBe("src/main.rs")
    expect(rewrite("notes.md")).toBe("notes.md")
    expect(rewrite("./index.html")).toBe("./index.html")
  })

  it("puts a root file position behind a slash so sanitize keeps it", () => {
    // `a.ts:12` reads as a URL with the scheme `a.ts:`; `./a.ts:12` does not.
    expect(rewrite("a.ts:12")).toBe("./a.ts:12")
    expect(rewrite("index.html:3:7")).toBe("./index.html:3:7")
    expect(rewrite(".env:2")).toBe("./.env:2")
    expect(rewrite(".env:3")).toBe("./.env:3")
    expect(rewrite("Makefile:40")).toBe("./Makefile:40")
    expect(rewrite("Makefile:40:2")).toBe("./Makefile:40:2")
    // With a directory the colon already sits behind a slash.
    expect(rewrite("src/a.ts:12")).toBe("src/a.ts:12")
  })

  it("leaves a host with a port, and other schemes, as they are", () => {
    for (const url of [
      "localhost:3000",
      "example.com:8080",
      "10.0.0.1:80",
      "tel:12345",
      "mailto:a@b.c",
      "a.ts:L12",
    ]) {
      expect(rewrite(url)).toBe(url)
    }
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

  it("rewrites a reference definition the same way", () => {
    const tree: Node = {
      type: "root",
      children: [
        {
          type: "paragraph",
          children: [
            {
              type: "linkReference",
              identifier: "pos",
              children: [{ type: "text" }],
            },
          ],
        },
        { type: "definition", identifier: "pos", url: "a.ts:12" },
      ],
    }
    remarkRewriteFileUriLinks()(tree)
    expect(tree.children![1].url).toBe("./a.ts:12")
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
