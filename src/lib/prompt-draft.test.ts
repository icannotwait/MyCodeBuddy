import { describe, expect, it } from "vitest"

import {
  buildUserMessageTextPartsFromDraft,
  extractUserImagesFromDraft,
  extractUserResourcesFromDraft,
  getPromptDraftDisplayText,
} from "@/lib/prompt-draft"
import type { PromptDraft } from "@/lib/types"

function draft(partial: Partial<PromptDraft>): PromptDraft {
  return { blocks: [], displayText: "", ...partial }
}

describe("getPromptDraftDisplayText", () => {
  it("returns the trimmed display text when present", () => {
    expect(
      getPromptDraftDisplayText(draft({ displayText: "  hello  " }), "fallback")
    ).toBe("hello")
  })

  it("returns the fallback when the display text is blank", () => {
    expect(
      getPromptDraftDisplayText(draft({ displayText: "   " }), "fallback")
    ).toBe("fallback")
  })
})

describe("buildUserMessageTextPartsFromDraft", () => {
  it("wraps the resolved display text in a single text part", () => {
    expect(
      buildUserMessageTextPartsFromDraft(draft({ displayText: "hi" }), "fb")
    ).toEqual([{ type: "text", text: "hi" }])
  })

  it("uses the fallback for an empty draft", () => {
    expect(buildUserMessageTextPartsFromDraft(draft({}), "fb")).toEqual([
      { type: "text", text: "fb" },
    ])
  })
})

describe("extractUserResourcesFromDraft", () => {
  it("maps resource_link blocks, preserving name and uri", () => {
    const result = extractUserResourcesFromDraft(
      draft({
        blocks: [
          {
            type: "resource_link",
            uri: "file:///a/b.txt",
            name: "b.txt",
            mime_type: "text/plain",
          },
        ],
      })
    )
    expect(result).toEqual([
      { name: "b.txt", uri: "file:///a/b.txt", mime_type: "text/plain" },
    ])
  })

  it("defaults missing mime_type to null", () => {
    const result = extractUserResourcesFromDraft(
      draft({
        blocks: [{ type: "resource_link", uri: "u", name: "n" }],
      })
    )
    expect(result[0].mime_type).toBeNull()
  })

  it("derives a name from the uri for embedded resource blocks", () => {
    const result = extractUserResourcesFromDraft(
      draft({
        blocks: [
          { type: "resource", uri: "file:///docs/report.pdf" },
          { type: "resource", uri: "https://x.com/dir/page.html?y=1#frag" },
        ],
      })
    )
    expect(result.map((r) => r.name)).toEqual(["report.pdf", "page.html"])
  })

  it("decodes percent-encoded resource names", () => {
    const result = extractUserResourcesFromDraft(
      draft({
        blocks: [{ type: "resource", uri: "file:///a/my%20file.txt" }],
      })
    )
    expect(result[0].name).toBe("my file.txt")
  })

  it("falls back to 'resource' for an empty or malformed uri", () => {
    const result = extractUserResourcesFromDraft(
      draft({
        blocks: [
          { type: "resource", uri: "   " },
          { type: "resource", uri: "file:///bad/%E0%A4%A.txt" },
        ],
      })
    )
    expect(result[0].name).toBe("resource")
    // Malformed percent-escape falls back to the raw candidate, not "resource".
    expect(result[1].name).toBe("%E0%A4%A.txt")
  })

  it("orders resource_link blocks before embedded resource blocks", () => {
    const result = extractUserResourcesFromDraft(
      draft({
        blocks: [
          { type: "resource", uri: "file:///embedded.txt" },
          { type: "resource_link", uri: "linked", name: "linked" },
        ],
      })
    )
    expect(result.map((r) => r.name)).toEqual(["linked", "embedded.txt"])
  })

  it("ignores unrelated block types", () => {
    const result = extractUserResourcesFromDraft(
      draft({ blocks: [{ type: "text", text: "hi" }] })
    )
    expect(result).toEqual([])
  })
})

describe("extractUserImagesFromDraft", () => {
  it("maps image blocks and derives a name from the uri", () => {
    const result = extractUserImagesFromDraft(
      draft({
        blocks: [
          {
            type: "image",
            data: "AAAA",
            mime_type: "image/png",
            uri: "file:///pics/cat.png",
          },
        ],
      })
    )
    expect(result).toEqual([
      {
        name: "cat.png",
        data: "AAAA",
        mime_type: "image/png",
        uri: "file:///pics/cat.png",
      },
    ])
  })

  it("derives a name from the mime type when there is no uri", () => {
    const result = extractUserImagesFromDraft(
      draft({
        blocks: [{ type: "image", data: "AAAA", mime_type: "image/jpeg" }],
      })
    )
    expect(result[0].name).toBe("image.jpeg")
    expect(result[0].uri).toBeNull()
  })

  it("strips structured-syntax suffixes from the mime subtype", () => {
    const result = extractUserImagesFromDraft(
      draft({
        blocks: [{ type: "image", data: "AAAA", mime_type: "image/svg+xml" }],
      })
    )
    expect(result[0].name).toBe("image.svg")
  })

  it("falls back to the mime type when the uri yields no usable name", () => {
    const result = extractUserImagesFromDraft(
      draft({
        blocks: [
          { type: "image", data: "AAAA", mime_type: "image/webp", uri: "   " },
        ],
      })
    )
    expect(result[0].name).toBe("image.webp")
  })

  it("defaults to 'image' subtype for a mime type without a subtype", () => {
    const result = extractUserImagesFromDraft(
      draft({
        blocks: [{ type: "image", data: "AAAA", mime_type: "image" }],
      })
    )
    expect(result[0].name).toBe("image.image")
  })

  it("ignores non-image blocks", () => {
    const result = extractUserImagesFromDraft(
      draft({ blocks: [{ type: "text", text: "hi" }] })
    )
    expect(result).toEqual([])
  })
})
