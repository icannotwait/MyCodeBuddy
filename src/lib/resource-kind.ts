/**
 * Classifies a markdown link / file address into a small set of "resource
 * kinds", purely for presentation: choosing the type icon shown before the
 * address in chat messages.
 *
 * The categories mirror how the link-safety click handler
 * (`useStreamdownLinkSafety` in `components/ai-elements/link-safety.tsx`)
 * actually routes a target, so the icon previews what a click will do:
 *   - `file`  → opened in the workspace file panel (file:// or a local path)
 *   - `web`   → opened in the browser (http / https)
 *   - `email` → mailto:
 *   - `phone` → tel:
 *
 * Returns `null` when no icon should be shown — in-page anchors (`#…`), the
 * streaming "incomplete link" placeholder, ungated bare-relative paths and
 * unknown schemes (`data:`, `vscode:`, `ftp:`, `javascript:`, …) that the
 * click handler can't act on, and empty input. Gated bare-relative paths
 * (incl. location-suffix forms like `docs/a.md:12` / `docs/a.md#L12`) are
 * now `file`, matching shared `isLocalPathLike` openability.
 *
 * The regexes intentionally match the ones in `link-safety.tsx`; this module
 * stays free of React/client imports so it remains pure and unit-testable.
 */

import { isLocalPathLike } from "@/lib/markdown/local-path-links"

export type ResourceKind = "file" | "web" | "email" | "phone"

const WINDOWS_ABSOLUTE_PATH = /^[a-zA-Z]:[\\/]/
const URL_SCHEME = /^([a-zA-Z][a-zA-Z\d+\-.]*):/

export function classifyResourceKind(rawUrl: string): ResourceKind | null {
  const trimmed = rawUrl.trim()
  if (!trimmed) return null

  // file:// URIs and Windows drive paths (C:\… / C:/…) are local files. The
  // drive-letter check must precede the generic scheme check below, because a
  // bare "C:" otherwise parses as a URL scheme.
  if (/^file:\/\//i.test(trimmed)) return "file"
  if (WINDOWS_ABSOLUTE_PATH.test(trimmed)) return "file"

  const scheme = trimmed.match(URL_SCHEME)?.[1]?.toLowerCase()
  if (scheme) {
    if (scheme === "mailto") return "email"
    if (scheme === "tel") return "phone"
    if (scheme === "http" || scheme === "https") return "web"
    // Unknown / unsupported scheme — the click handler can't open it, so don't
    // imply a type.
    return null
  }

  // Protocol-relative "//host/path" routes to the browser (link-safety
  // resolves it against the page protocol) — web icon.
  if (trimmed.startsWith("//")) return "web"

  // Schemeless local targets — absolute (/…), explicitly-relative (./ ../ ~/),
  // UNC, Windows drive, and gated bare-relative (src/main.rs, docs/a.md:12)
  // — are routed to the workspace file opener by link-safety's shared
  // `isLocalPathLike`, so they get the file icon to match click behavior.
  // Hostname-like bare paths and extensionless bare relatives stay untagged.
  if (isLocalPathLike(trimmed)) return "file"

  return null
}
