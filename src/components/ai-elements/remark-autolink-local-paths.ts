import {
  findAbsoluteLocalPathRanges,
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
  "inlineCode",
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
  const matches = findAbsoluteLocalPathRanges(node.value)
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
