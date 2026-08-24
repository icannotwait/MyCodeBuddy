import type { ComponentProps } from "react"
import type { Streamdown } from "streamdown"

import { isLocalPathLike } from "@/lib/markdown/local-path-links"
import { parseGrokSessionImageRef } from "@/lib/markdown/grok-session-image"

type RehypePlugins = NonNullable<
  ComponentProps<typeof Streamdown>["rehypePlugins"]
>
type RehypePlugin = RehypePlugins[number]

export type RehypeAllowCodegOptions = {
  grokSessionImages?: boolean
}

/** Minimal view of rehype-sanitize's schema — only the protocol allow-list we widen. */
type SanitizeSchema = {
  protocols?: Record<string, string[]>
  [key: string]: unknown
}

type HastNode = {
  type?: string
  tagName?: string
  properties?: {
    href?: unknown
    src?: unknown
    alt?: unknown
    [key: string]: unknown
  }
  children?: HastNode[]
}

/** Placeholder absolute URL rehype-harden will always accept. */
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
 * string and opted-in Grok image refs can be retagged after hardening. Harden
 * only sees safe placeholders; originals are restored on the same nodes.
 */
function wrapHardenPreservingLocalPaths(
  hardenEntry: RehypePlugin,
  allowOptions?: RehypeAllowCodegOptions
): RehypePlugin {
  const [hardenFn, hardenOptions] = (
    Array.isArray(hardenEntry) ? hardenEntry : [hardenEntry]
  ) as [(opts?: unknown) => (tree: HastNode) => void, unknown?]

  const createTransform = () => {
    const runHarden = hardenFn(hardenOptions)
    return (tree: HastNode) => {
      const preservedHrefs = new Map<HastNode, string>()
      const preservedImages = new Map<HastNode, { src: string; alt: unknown }>()
      walkElements(tree, (node) => {
        if (node.tagName === "a") {
          const href = node.properties?.href
          if (typeof href === "string" && shouldPreserveLocalPathHref(href)) {
            preservedHrefs.set(node, href)
            if (node.properties) node.properties.href = PRESERVE_PLACEHOLDER
          }
          return
        }

        if (
          node.tagName !== "img" ||
          allowOptions?.grokSessionImages !== true
        ) {
          return
        }
        const src = node.properties?.src
        if (typeof src !== "string" || !parseGrokSessionImageRef(src)) return
        preservedImages.set(node, { src, alt: node.properties?.alt })
        if (node.properties) node.properties.src = PRESERVE_PLACEHOLDER
      })

      runHarden(tree)

      for (const [node, href] of preservedHrefs) {
        if (node.tagName === "a" && node.properties) {
          node.properties.href = href
        }
      }
      for (const [node, { src, alt }] of preservedImages) {
        node.tagName = "codeg-grok-session-image"
        node.properties = {
          src,
          ...(typeof alt === "string" ? { alt } : {}),
        }
      }
    }
  }

  const hardenPlugin = function rehypeHardenPreservingLocalPaths() {
    return createTransform()
  }

  // Streamdown 2.2 keys processors by plugin.name plus serialized tuple
  // options. Production minification can erase function names, so encode the
  // security scope in the stable tuple data instead of relying on the name.
  return [
    hardenPlugin,
    {
      codegCacheScope:
        allowOptions?.grokSessionImages === true
          ? "grok-session-images"
          : "ordinary-markdown",
    },
  ] as RehypePlugin
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
  defaults: Record<string, RehypePlugin>,
  options?: RehypeAllowCodegOptions
): RehypePlugins {
  return Object.entries(defaults).map<RehypePlugin>(([key, plugin]) => {
    if (key === "harden") {
      return wrapHardenPreservingLocalPaths(plugin, options)
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
