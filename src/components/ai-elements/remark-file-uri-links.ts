// rehype-harden hard-codes `file:` in its blocked-protocol list and replaces
// such links with `<span>… [blocked]</span>`. Rewriting `file://` hrefs in
// the mdast layer (before remark-rehype) sidesteps the block while keeping
// the link clickable through the existing link-safety + open-file-dialog
// flow. Image syntax is intentionally left untouched: harden's
// "[Image blocked: …]" placeholder is more useful than a broken <img src>.
//
// Bare Windows drive destinations (`D:/…`, `C:\…`) hit the same wall: the
// parser treats the drive letter as a URL scheme, harden blocks it, and the
// transcript shows `label [blocked]`. Prefixing `/` yields the same
// root-relative shape `file://` rewrite already emits (`/D:/…`), which harden
// allows and `parseLocalFileTarget` understands.

type MdastNodeLike = {
  type: string
  url?: unknown
  identifier?: unknown
  children?: unknown
}

const BARE_WINDOWS_DRIVE = /^[a-zA-Z]:[\\/]/

function fileUriToLocalPath(uri: string): string | null {
  if (!/^file:\/\//i.test(uri)) return null
  let parsed: URL
  try {
    parsed = new URL(uri)
  } catch {
    return null
  }
  // A non-empty host is a UNC authority: file://server/share/x parses as
  // host="server", pathname="/share/x". Emit the BACKSLASH UNC form
  // \\server\share\x — unambiguously LOCAL. A forward-slash //server/share
  // would be indistinguishable from a protocol-relative WEB url once the
  // file: scheme is gone, and downstream (classifyResourceKind /
  // link-safety) route bare // to the browser; backslashes never appear in
  // a web url, so they reliably tag the target as a local file. The click
  // path normalizes the separators back to // before opening.
  if (parsed.host) {
    const body = `${parsed.host}${parsed.pathname}`.replace(/\//g, "\\")
    return `\\\\${body}${parsed.search}${parsed.hash}`
  }
  // Keep the leading slash on Windows drive paths. `/C:/x` survives harden as a
  // root-relative href, and parseLocalFileTarget strips that slash on click.
  // The URL parser already preserves encoded path characters in pathname.
  const path = parsed.pathname
  return `${path}${parsed.search}${parsed.hash}`
}

/** Prefix `/` on bare `D:/…` / `D:\…` so harden does not treat the drive as a scheme. */
function windowsDriveHrefToHardenSafe(url: string): string | null {
  if (!BARE_WINDOWS_DRIVE.test(url)) return null
  return `/${url.replace(/\\/g, "/")}`
}

function rewriteLocalLinkUrl(url: string): string | null {
  return fileUriToLocalPath(url) ?? windowsDriveHrefToHardenSafe(url)
}

function walk(node: MdastNodeLike, fn: (n: MdastNodeLike) => void): void {
  fn(node)
  const { children } = node
  if (Array.isArray(children)) {
    for (const child of children) {
      walk(child as MdastNodeLike, fn)
    }
  }
}

export function remarkRewriteFileUriLinks() {
  return (tree: MdastNodeLike) => {
    // Definitions are shared between linkReference and imageReference. Skip
    // any definition whose identifier is consumed by an imageReference so
    // image blocking still wins for those cases.
    const imageRefIds = new Set<string>()
    walk(tree, (node) => {
      if (
        node.type === "imageReference" &&
        typeof node.identifier === "string"
      ) {
        imageRefIds.add(node.identifier.toLowerCase())
      }
    })

    walk(tree, (node) => {
      if (typeof node.url !== "string") return
      if (node.type === "link") {
        const rewritten = rewriteLocalLinkUrl(node.url)
        if (rewritten != null) node.url = rewritten
        return
      }
      if (node.type === "definition") {
        const id =
          typeof node.identifier === "string"
            ? node.identifier.toLowerCase()
            : ""
        if (imageRefIds.has(id)) return
        const rewritten = rewriteLocalLinkUrl(node.url)
        if (rewritten != null) node.url = rewritten
      }
    })
  }
}
