import type { ComponentProps } from "react"
import type { Streamdown } from "streamdown"

import { isLocalPathLike } from "@/lib/markdown/local-path-links"

type RehypePlugins = NonNullable<
  ComponentProps<typeof Streamdown>["rehypePlugins"]
>
type RehypePlugin = RehypePlugins[number]

/** Minimal view of rehype-sanitize's schema — only the protocol allow-list we widen. */
type SanitizeSchema = {
  protocols?: Record<string, string[]>
  [key: string]: unknown
}

type HastNode = {
  type?: string
  tagName?: string
  properties?: { href?: unknown; [key: string]: unknown }
  children?: HastNode[]
}

/** Placeholder absolute URL rehype-harden will always accept and leave as an `<a>`. */
const PRESERVE_PLACEHOLDER = "https://__codeg.local__/preserve"

function decodeUriSafely(value: string): string {
  try {
    return decodeURIComponent(value)
  } catch {
    return value
  }
}

/**
 * True when an href should skip rehype-harden's rewrite/block path.
 *
 * Bare relatives (`docs/a.md`) fail harden's relative-URL parse (only `/`,
 * `./`, `../` are retried). Explicit relatives (`./x`, `../x`) are accepted
 * but reconstructed as root pathnames (`/x`), which then open as POSIX
 * absolutes. Local path-like hrefs must reach MarkdownLink / parseLocalFileTarget
 * with their original shape.
 */
export function shouldPreserveLocalPathHref(href: string): boolean {
  if (!href) return false
  if (isLocalPathLike(href)) return true
  const decoded = decodeUriSafely(href)
  return decoded !== href && isLocalPathLike(decoded)
}

function walkElements(node: HastNode, visit: (el: HastNode) => void): void {
  if (node.type === "element") visit(node)
  if (!Array.isArray(node.children)) return
  for (const child of node.children) walkElements(child, visit)
}

/**
 * Wrap Streamdown's `[harden, options]` so local-path hrefs keep their original
 * string through harden. Harden only sees a safe placeholder for those anchors;
 * originals are restored afterward on the same element nodes.
 */
function wrapHardenPreservingLocalPaths(
  hardenEntry: RehypePlugin
): RehypePlugin {
  const [hardenFn, options] = (
    Array.isArray(hardenEntry) ? hardenEntry : [hardenEntry]
  ) as [(opts?: unknown) => (tree: HastNode) => void, unknown?]

  return function rehypeHardenPreservingLocalPaths() {
    const runHarden = hardenFn(options)
    return (tree: HastNode) => {
      const preserved = new Map<HastNode, string>()
      walkElements(tree, (node) => {
        if (node.tagName !== "a") return
        const href = node.properties?.href
        if (typeof href !== "string" || !shouldPreserveLocalPathHref(href)) {
          return
        }
        preserved.set(node, href)
        if (node.properties) node.properties.href = PRESERVE_PLACEHOLDER
      })

      runHarden(tree)

      for (const [node, href] of preserved) {
        if (node.tagName === "a" && node.properties) {
          node.properties.href = href
        }
      }
    }
  }
}

/**
 * Re-derive Streamdown's default rehype pipeline so the app-internal `codeg`
 * scheme survives sanitization and reaches `MarkdownLink` → `ReferenceBadge`,
 * and so workspace local-path hrefs (absolute + relative) survive rehype-harden
 * without being blocked or rewritten to root pathnames.
 *
 * Streamdown's default pipeline is `[raw, [rehypeSanitize, schema], harden]`
 * (run in that order). The sanitize schema's `protocols.href` allow-list omits
 * `codeg`, so it strips the href off our `[label](codeg://…)` reference links;
 * rehype-harden then sees a hrefless `<a>`, can't transform it, and replaces it
 * with a `… [blocked]` span — all at the rehype stage, *before* react-markdown
 * maps `<a>` to `MarkdownLink` (which turns a `codeg:` href into an inline
 * badge). The net effect was `@Codex CLI [blocked]` in the transcript.
 *
 * Adding `codeg` to the sanitize allow-list lets the href survive. harden still
 * hard-blocks `javascript:` / `data:` / `file:` / `vbscript:`. Local path-like
 * hrefs are temporarily swapped for a safe placeholder before harden runs and
 * restored afterward so bare / `./` / `../` workspace paths keep their Task 1
 * safe-href shape for `parseLocalFileTarget`.
 *
 * `file://` links are unaffected — they are rewritten to local paths at the
 * remark layer (see {@link "./remark-file-uri-links"}) before sanitize runs.
 *
 * Only the `sanitize` and `harden` entries are rewritten; every other plugin is
 * passed through in its original position (mirroring how Streamdown builds the
 * default list via `Object.values`).
 */
export function rehypePluginsAllowingCodeg(
  defaults: Record<string, RehypePlugin>
): RehypePlugins {
  return Object.entries(defaults).map<RehypePlugin>(([key, plugin]) => {
    if (key === "harden") {
      return wrapHardenPreservingLocalPaths(plugin)
    }
    if (key !== "sanitize") return plugin
    const [sanitizePlugin, schema] = (
      Array.isArray(plugin) ? plugin : [plugin]
    ) as [RehypePlugin, SanitizeSchema?]
    const href = schema?.protocols?.href ?? []
    const next: SanitizeSchema = {
      ...schema,
      protocols: {
        ...schema?.protocols,
        href: href.includes("codeg") ? href : [...href, "codeg"],
      },
    }
    return [sanitizePlugin, next] as RehypePlugin
  })
}
