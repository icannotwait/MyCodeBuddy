import { describe, expect, it, vi } from "vitest"
import { parseMarkdownIntoBlocks } from "streamdown"
import {
  appendStreamingMarkdown,
  completeStreamingMarkdown,
  createIncrementalStreamBlocks,
  joinStreamingMarkdown,
  sealStreamingMarkdownBoundary,
} from "./incremental-stream-blocks"

function splitAtEveryCodeUnit(source: string): string[] {
  return Array.from({ length: source.length }, (_, index) =>
    source.slice(index, index + 1)
  )
}

describe("incremental-stream-blocks", () => {
  it.each([
    ["paragraphs", "one\n\ntwo\n\nthree"],
    ["backtick fence", "before\n\n```ts\nconst x = 1\n```\n\nafter"],
    ["tilde fence", "~~~js\nalert(1)\n~~~\n\nend"],
    ["table", "| a | b |\n| - | - |\n| 1 | 2 |\n\nend"],
    ["math", "text $x$\n\n$$\ny = x\n$$\n"],
    ["CJK", "第一段。\n\n第二段。"],
    ["HTML", "<details>\n<summary>x</summary>\ny\n</details>\n\nend"],
  ] as const)("reconstructs exact source for %s", (_name, source) => {
    let document = createIncrementalStreamBlocks("segment-1")
    for (const chunk of splitAtEveryCodeUnit(source)) {
      document = appendStreamingMarkdown(document, chunk)
    }
    document = completeStreamingMarkdown(document)
    expect(joinStreamingMarkdown(document)).toBe(source)
    expect(document.tail).toBe("")
    expect(document.valid).toBe(true)
  })

  it("does not repeatedly parse a long unclosed fence", () => {
    const split = vi.fn(parseMarkdownIntoBlocks)
    let document = createIncrementalStreamBlocks("segment-1", split)
    document = appendStreamingMarkdown(document, "```ts\n")
    for (let index = 0; index < 2_000; index += 1) {
      document = appendStreamingMarkdown(document, `line-${index}\n`)
    }
    expect(split.mock.calls.length).toBeLessThanOrEqual(1)
    expect(document.sealed).toHaveLength(0)
    expect(document.tail).toContain("line-1999")
  })

  it("seals safe blocks but keeps an unmatched tail at a tool boundary", () => {
    let document = createIncrementalStreamBlocks("segment-1")
    document = appendStreamingMarkdown(document, "done\n\n**unfinished")
    document = sealStreamingMarkdownBoundary(document)
    expect(document.sealed.map((block) => block.markdown).join("")).toBe(
      "done\n\n"
    )
    expect(document.tail).toBe("**unfinished")
  })

  it("seals a closed fence without waiting for a following block", () => {
    let document = createIncrementalStreamBlocks("segment-1")
    document = appendStreamingMarkdown(document, "```ts\nconst x = 1\n```\n")
    expect(document.sealed.map((block) => block.markdown).join("")).toBe(
      "```ts\nconst x = 1\n```\n"
    )
    expect(document.tail).toBe("")
  })

  it("does not treat a backtick-prefixed code line as a closing fence", () => {
    let document = createIncrementalStreamBlocks("segment-1")
    document = appendStreamingMarkdown(
      document,
      "```ts\n```not-a-close\nconst x = 1\n"
    )
    expect(document.scanner.fence).not.toBeNull()
    expect(document.sealed).toHaveLength(0)
  })

  it("invalid Markdown partition falls back to visible canonical source", () => {
    // split returns pieces that do not rejoin to the scanned prefix.
    const brokenSplit = vi.fn(() => ["partial"])
    let document = createIncrementalStreamBlocks("segment-1", brokenSplit)
    document = appendStreamingMarkdown(document, "visible canonical\n\n")
    expect(document.valid).toBe(false)
    expect(joinStreamingMarkdown(document)).toContain("visible canonical")
  })
})
