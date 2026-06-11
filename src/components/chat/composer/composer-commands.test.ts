import { Editor } from "@tiptap/core"
import { afterEach, beforeEach, describe, expect, it } from "vitest"

import { applyExpertPrefix, isComposerEmpty } from "./composer-commands"
import { buildComposerExtensions } from "./editor-config"

describe("isComposerEmpty", () => {
  let editor: Editor

  beforeEach(() => {
    editor = new Editor({ extensions: buildComposerExtensions() })
  })
  afterEach(() => editor?.destroy())

  it("is true for an empty document", () => {
    expect(isComposerEmpty(editor)).toBe(true)
  })

  it("is false once there is real text", () => {
    editor.commands.setContent("hello", { contentType: "markdown" })
    expect(isComposerEmpty(editor)).toBe(false)
  })

  it("is true for a whitespace-only document (regression: send stays disabled)", () => {
    editor.commands.insertContent("    ")
    expect(editor.isEmpty).toBe(false) // ProseMirror itself reports non-empty…
    expect(isComposerEmpty(editor)).toBe(true) // …but there's nothing to send.
  })

  it("is false for a document holding only a reference badge", () => {
    editor.commands.insertReference({
      refType: "file",
      id: "a.ts",
      label: "a.ts",
      uri: "file:///a.ts",
      meta: null,
    })
    expect(editor.isEmpty).toBe(false)
    expect(isComposerEmpty(editor)).toBe(false)
  })
})

describe("applyExpertPrefix", () => {
  let editor: Editor

  beforeEach(() => {
    editor = new Editor({ extensions: buildComposerExtensions() })
  })
  afterEach(() => editor?.destroy())

  it("prepends the prefix to an empty document", () => {
    applyExpertPrefix(editor, "/", "reviewer", new Set())
    expect(editor.getMarkdown().trimStart()).toMatch(/^\/reviewer\b/)
  })

  it("prepends the prefix in front of existing prose", () => {
    editor.commands.setContent("look at this", { contentType: "markdown" })
    applyExpertPrefix(editor, "/", "reviewer", new Set())
    expect(editor.getMarkdown().trimStart()).toMatch(/^\/reviewer look at this/)
  })

  it("replaces an existing known expert prefix instead of stacking", () => {
    editor.commands.setContent("/old keep this", { contentType: "markdown" })
    applyExpertPrefix(editor, "/", "reviewer", new Set(["old"]))
    const md = editor.getMarkdown()
    expect(md.trimStart()).toMatch(/^\/reviewer keep this/)
    expect(md).not.toContain("old")
  })

  it("does NOT replace a leading token that isn't a known expert", () => {
    editor.commands.setContent("/unknown keep", { contentType: "markdown" })
    applyExpertPrefix(editor, "/", "reviewer", new Set(["old"]))
    const md = editor.getMarkdown()
    expect(md.trimStart()).toMatch(/^\/reviewer /)
    expect(md).toContain("/unknown")
  })

  it("keeps the prefix ahead of a heading's Markdown marker (regression)", () => {
    // First block is a heading: inserting inline at pos 1 would serialize as
    // `# /reviewer Title` (marker first). The prefix must lead the message.
    editor.commands.setContent("# Title", { contentType: "markdown" })
    applyExpertPrefix(editor, "/", "reviewer", new Set())
    const md = editor.getMarkdown()
    expect(md.trimStart()).toMatch(/^\/reviewer/)
    expect(md).toContain("# Title")
    expect(md.indexOf("/reviewer")).toBeLessThan(md.indexOf("# Title"))
  })

  it("keeps the prefix ahead of a list's Markdown marker", () => {
    editor.commands.setContent("- one\n- two", { contentType: "markdown" })
    applyExpertPrefix(editor, "/", "reviewer", new Set())
    const md = editor.getMarkdown()
    expect(md.trimStart()).toMatch(/^\/reviewer/)
    expect(md.indexOf("/reviewer")).toBeLessThan(md.indexOf("one"))
  })

  it("supports the Codex `$` prefix", () => {
    editor.commands.setContent("ship it", { contentType: "markdown" })
    applyExpertPrefix(editor, "$", "deploy", new Set())
    expect(editor.getMarkdown().trimStart()).toMatch(/^\$deploy ship it/)
  })
})
