import type { JSONContent } from "@tiptap/core"

/**
 * Convert a plain-text string into Tiptap inline content: literal text with each
 * `\n` turned into a `hardBreak` node. The plain-text composer schema has no code
 * block to hold a literal newline, so line breaks are hard breaks — which
 * {@link "./to-prompt-blocks".serializeDocToText} maps back to `\n`, so the text
 * round-trips. An empty string yields an empty array.
 *
 * Used wherever the host seeds the composer from plain text (drafts, quick
 * messages, expert/office prompt templates, injected content) now that no
 * Markdown parser is loaded.
 */
export function textToInlineContent(text: string): JSONContent[] {
  if (!text) return []
  const out: JSONContent[] = []
  const lines = text.split("\n")
  lines.forEach((line, index) => {
    if (index > 0) out.push({ type: "hardBreak" })
    // A ProseMirror text node may not be empty, so a blank line contributes only
    // its hardBreak (two adjacent breaks = one blank line).
    if (line.length > 0) out.push({ type: "text", text: line })
  })
  return out
}

/**
 * A whole document (one paragraph) holding {@link textToInlineContent}. Used to
 * replace the composer content from a plain-text string.
 */
export function textToDoc(text: string): JSONContent {
  return {
    type: "doc",
    content: [{ type: "paragraph", content: textToInlineContent(text) }],
  }
}
