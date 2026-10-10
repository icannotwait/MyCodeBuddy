import type { JSONContent } from "@tiptap/core"

/**
 * Serialize the roundtable topic editor to the plain text the room stores.
 *
 * A file badge becomes `@relative/path` (a folder gets a trailing `/`). Members
 * read that path themselves inside the read-only `/workspace-ro` bind, so the
 * topic never carries a `file://` uri or any absolute host path. Every other
 * badge kind degrades to its label: the roundtable has no agent, session or
 * skill routing.
 */
export function roundtableTopicFromDoc(doc: JSONContent | null | undefined) {
  if (!doc) return ""
  const paragraphs = (doc.content ?? []).map((block) => inlineText(block))
  return paragraphs.join("\n")
}

/** Workspace-relative paths referenced by file badges, in order, deduplicated. */
export function roundtableTopicReferences(
  doc: JSONContent | null | undefined
): string[] {
  const seen = new Set<string>()
  const walk = (node: JSONContent) => {
    if (node.type === "reference") {
      const path = fileReferencePath(node.attrs)
      if (path) seen.add(path)
    }
    for (const child of node.content ?? []) walk(child)
  }
  if (doc) walk(doc)
  return [...seen]
}

function inlineText(node: JSONContent): string {
  switch (node.type) {
    case "text":
      return node.text ?? ""
    case "hardBreak":
      return "\n"
    case "reference": {
      const path = fileReferencePath(node.attrs)
      if (path) return `@${path}`
      const label = node.attrs?.label ?? node.attrs?.id
      return typeof label === "string" ? label : ""
    }
    default:
      return (node.content ?? []).map(inlineText).join("")
  }
}

/**
 * The badge's workspace-relative path, or null when it is not a file badge or
 * its id is not a safe relative path (absolute, drive-qualified, `..`).
 */
export function fileReferencePath(
  attrs: Record<string, unknown> | undefined
): string | null {
  if (!attrs || attrs.refType !== "file") return null
  const raw = typeof attrs.id === "string" ? attrs.id : ""
  const path = raw.replaceAll("\\", "/").replace(/\/+$/, "")
  if (
    !path ||
    path.startsWith("/") ||
    /^[a-zA-Z]:/.test(path) ||
    path.includes("\0") ||
    path.split("/").some((part) => part === ".." || part === "")
  ) {
    return null
  }
  const meta = attrs.meta as { fileKind?: string } | null | undefined
  return meta?.fileKind === "dir" ? `${path}/` : path
}
