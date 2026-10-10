import { describe, expect, it } from "vitest"

import {
  fileReferencePath,
  roundtableTopicFromDoc,
  roundtableTopicReferences,
} from "./topic"

const file = (id: string, fileKind = "file") => ({
  type: "reference",
  attrs: {
    refType: "file",
    id,
    label: id.split("/").pop(),
    uri: `file:///home/someone/project/${id}`,
    meta: { fileKind },
  },
})

describe("roundtable topic serialization", () => {
  it("turns file badges into @relative/path and never leaks the uri", () => {
    const doc = {
      type: "doc",
      content: [
        {
          type: "paragraph",
          content: [
            { type: "text", text: "Review " },
            file("src/lib/api.ts"),
            { type: "text", text: " and " },
            file("docs", "dir"),
          ],
        },
        {
          type: "paragraph",
          content: [{ type: "text", text: "second line" }],
        },
      ],
    }
    const text = roundtableTopicFromDoc(doc)
    expect(text).toBe("Review @src/lib/api.ts and @docs/\nsecond line")
    expect(text).not.toContain("file://")
    expect(text).not.toContain("/home/")
    expect(roundtableTopicReferences(doc)).toEqual(["src/lib/api.ts", "docs/"])
  })

  it("degrades other badges to their label", () => {
    const doc = {
      type: "doc",
      content: [
        {
          type: "paragraph",
          content: [
            {
              type: "reference",
              attrs: { refType: "agent", id: "grok", label: "Grok" },
            },
            { type: "hardBreak" },
            { type: "text", text: "x" },
          ],
        },
      ],
    }
    expect(roundtableTopicFromDoc(doc)).toBe("Grok\nx")
    expect(roundtableTopicReferences(doc)).toEqual([])
  })

  it("rejects unsafe file ids", () => {
    for (const id of ["/etc/passwd", "../up", "a/../b", "C:/x", "a//b", ""]) {
      expect(fileReferencePath({ refType: "file", id })).toBeNull()
    }
    expect(fileReferencePath({ refType: "file", id: "a\\b.ts" })).toBe("a/b.ts")
  })
})
