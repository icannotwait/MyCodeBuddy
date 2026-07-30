import {
  findLocalPathRanges,
  toSafeLocalPathHref,
} from "@/lib/markdown/local-path-links"

type MdastNodeLike = {
  type: string
  value?: unknown
  url?: unknown
  children?: MdastNodeLike[]
  position?: {
    start?: { offset?: number }
    end?: { offset?: number }
  }
}

type VFileLike = { value?: unknown }

const SKIP_SUBTREES = new Set([
  "link",
  "linkReference",
  "code",
  "html",
  "image",
  "imageReference",
  "definition",
])

function hasExactSourceMapping(
  node: MdastNodeLike,
  source: string | null | undefined
): boolean {
  if (source === null) return true
  if (typeof source !== "string") return false
  const start = node.position?.start?.offset
  const end = node.position?.end?.offset
  return (
    typeof start === "number" &&
    typeof end === "number" &&
    source.slice(start, end) === node.value
  )
}

function linkifyTextNode(
  node: MdastNodeLike,
  source: string | null | undefined
): MdastNodeLike[] {
  if (typeof node.value !== "string") return [node]
  if (!hasExactSourceMapping(node, source)) return [node]
  const matches = findLocalPathRanges(node.value)
  if (matches.length === 0) return [node]

  const replacement: MdastNodeLike[] = []
  let cursor = 0
  for (const match of matches) {
    if (match.start > cursor) {
      replacement.push({
        type: "text",
        value: node.value.slice(cursor, match.start),
      })
    }
    const href = toSafeLocalPathHref(match)
    replacement.push(
      href
        ? {
            type: "link",
            url: href,
            children: [{ type: "text", value: match.label }],
          }
        : { type: "text", value: match.label }
    )
    cursor = match.end
  }
  if (cursor < node.value.length) {
    replacement.push({ type: "text", value: node.value.slice(cursor) })
  }
  return replacement
}

/**
 * Agents often wrap a single file path in backticks. Convert only when the
 * entire inlineCode value is one local-path match (same gates as prose) so
 * snippets like `const x = src/app.ts` stay code.
 */
function linkifyInlineCodeNode(node: MdastNodeLike): MdastNodeLike {
  if (typeof node.value !== "string") return node
  const value = node.value
  const matches = findLocalPathRanges(value)
  if (matches.length !== 1) return node
  const match = matches[0]
  if (match.start !== 0 || match.end !== value.length) return node
  const href = toSafeLocalPathHref(match)
  if (!href) return node
  return {
    type: "link",
    url: href,
    children: [{ type: "text", value: match.label }],
  }
}

function transformChildren(
  node: MdastNodeLike,
  source: string | null | undefined
): void {
  if (SKIP_SUBTREES.has(node.type) || !Array.isArray(node.children)) return
  const nextChildren: MdastNodeLike[] = []
  for (const child of node.children) {
    if (child.type === "text") {
      nextChildren.push(...linkifyTextNode(child, source))
      continue
    }
    if (child.type === "inlineCode") {
      nextChildren.push(linkifyInlineCodeNode(child))
      continue
    }
    transformChildren(child, source)
    nextChildren.push(child)
  }
  node.children = nextChildren
}

export function remarkAutolinkLocalPaths() {
  return (tree: MdastNodeLike, file?: VFileLike) => {
    const source =
      file === undefined
        ? null
        : typeof file.value === "string"
          ? file.value
          : undefined
    transformChildren(tree, source)
  }
}
