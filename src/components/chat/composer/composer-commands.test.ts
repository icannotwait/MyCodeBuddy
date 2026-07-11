import { Editor } from "@tiptap/core"
import { afterEach, beforeEach, describe, expect, it } from "vitest"

import type { PromptInputBlock } from "@/lib/types"

import {
  applyExpertReference,
  isComposerChromeClick,
  isComposerEmpty,
  restoreBlocksIntoEditor,
} from "./composer-commands"
import { buildComposerExtensions } from "./editor-config"
import { serializeDocToText } from "./to-prompt-blocks"
import type { ReferenceAttrs } from "./types"

/** The composer's plain-text send serialization (references → inline tokens). */
function serialized(editor: Editor): string {
  return serializeDocToText(editor.state.doc)
}

/** An expert reference (refType `skill`, `meta.scope === "expert"`). */
function expertAttrs(id: string, prefix: "/" | "$" = "/"): ReferenceAttrs {
  return {
    refType: "skill",
    id,
    label: id,
    uri: null,
    meta: { scope: "expert", invocationPrefix: prefix },
  }
}

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
    editor.commands.setContent("hello")
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

describe("isComposerChromeClick", () => {
  it("treats a click on bare chrome (a plain div) as an empty-chrome click", () => {
    expect(isComposerChromeClick(document.createElement("div"))).toBe(true)
  })

  it("excludes interactive controls and their descendants", () => {
    const button = document.createElement("button")
    const icon = document.createElement("span")
    button.appendChild(icon)
    expect(isComposerChromeClick(button)).toBe(false)
    // closest() walks up, so a click on the button's icon is excluded too.
    expect(isComposerChromeClick(icon)).toBe(false)

    const roleButton = document.createElement("div")
    roleButton.setAttribute("role", "button")
    expect(isComposerChromeClick(roleButton)).toBe(false)
  })

  it("excludes the editor surface and inline badges", () => {
    const pm = document.createElement("div")
    pm.className = "ProseMirror"
    expect(isComposerChromeClick(pm)).toBe(false)

    const badge = document.createElement("span")
    badge.setAttribute("data-reference-badge", "")
    expect(isComposerChromeClick(badge)).toBe(false)
  })

  it("returns false for null / non-Element targets", () => {
    expect(isComposerChromeClick(null)).toBe(false)
    expect(isComposerChromeClick(document)).toBe(false)
  })
})

describe("applyExpertReference", () => {
  let editor: Editor

  beforeEach(() => {
    editor = new Editor({ extensions: buildComposerExtensions() })
  })
  afterEach(() => editor?.destroy())

  it("prepends an expert badge to an empty document", () => {
    applyExpertReference(editor, expertAttrs("reviewer"))
    // The badge is a real reference node (not plain text)…
    expect(JSON.stringify(editor.getJSON())).toContain('"refType":"skill"')
    // …that serializes to its `/reviewer` invocation token at the front.
    expect(serialized(editor).trimStart()).toMatch(/^\/reviewer\b/)
  })

  it("prepends the badge in front of existing prose", () => {
    editor.commands.setContent("look at this")
    applyExpertReference(editor, expertAttrs("reviewer"))
    expect(serialized(editor).trimStart()).toMatch(/^\/reviewer look at this/)
  })

  it("replaces an existing leading expert badge instead of stacking", () => {
    applyExpertReference(editor, expertAttrs("old"))
    applyExpertReference(editor, expertAttrs("reviewer"))
    const md = serialized(editor)
    expect(md.trimStart()).toMatch(/^\/reviewer\b/)
    expect(md).not.toContain("/old")
    // Exactly one expert badge remains.
    expect(
      JSON.stringify(editor.getJSON()).match(/"refType":"skill"/g)
    ).toHaveLength(1)
  })

  it("does NOT replace a leading plain-text token (only a real expert badge)", () => {
    editor.commands.setContent("/unknown keep")
    applyExpertReference(editor, expertAttrs("reviewer"))
    const md = serialized(editor)
    expect(md.trimStart()).toMatch(/^\/reviewer /)
    expect(md).toContain("/unknown")
  })

  it("supports the Codex `$` prefix", () => {
    editor.commands.setContent("ship it")
    applyExpertReference(editor, expertAttrs("deploy", "$"))
    expect(serialized(editor).trimStart()).toMatch(/^\$deploy ship it/)
  })
})

describe("restoreBlocksIntoEditor", () => {
  let editor: Editor

  beforeEach(() => {
    editor = new Editor({ extensions: buildComposerExtensions() })
  })
  afterEach(() => editor?.destroy())

  it("restores prose from a text block (no attachments)", () => {
    const blocks: PromptInputBlock[] = [
      { type: "text", text: "hello **world**" },
    ]
    const attachments = restoreBlocksIntoEditor(editor, blocks)
    expect(serialized(editor)).toContain("**world**")
    expect(attachments).toEqual([])
  })

  it("restores a file resource_link as a reference badge", () => {
    const blocks: PromptInputBlock[] = [
      { type: "text", text: "see" },
      {
        type: "resource_link",
        uri: "file:///repo/app.ts",
        name: "app.ts",
        mime_type: null,
        description: null,
      },
    ]
    const attachments = restoreBlocksIntoEditor(editor, blocks)
    expect(JSON.stringify(editor.getJSON())).toContain('"type":"reference"')
    expect(serialized(editor)).toContain("see")
    expect(attachments).toEqual([])
  })

  it("restores a non-composer resource_link as an attachment, not a badge", () => {
    const blocks: PromptInputBlock[] = [
      {
        type: "resource_link",
        uri: "https://example.com/x.pdf",
        name: "x.pdf",
        mime_type: "application/pdf",
        description: null,
      },
    ]
    const attachments = restoreBlocksIntoEditor(editor, blocks)
    expect(JSON.stringify(editor.getJSON())).not.toContain('"type":"reference"')
    expect(attachments).toHaveLength(1)
    expect(attachments[0]).toMatchObject({
      type: "resource",
      kind: "link",
      uri: "https://example.com/x.pdf",
    })
  })

  it("returns image blocks as attachments (not editor content)", () => {
    const blocks: PromptInputBlock[] = [
      { type: "image", data: "BASE64", mime_type: "image/png", uri: null },
    ]
    const attachments = restoreBlocksIntoEditor(editor, blocks)
    expect(attachments).toHaveLength(1)
    expect(attachments[0]).toMatchObject({ type: "image", data: "BASE64" })
  })

  it("clears any prior content before restoring", () => {
    editor.commands.setContent("stale draft")
    restoreBlocksIntoEditor(editor, [{ type: "text", text: "fresh" }])
    const md = serialized(editor)
    expect(md).toContain("fresh")
    expect(md).not.toContain("stale")
  })
})
