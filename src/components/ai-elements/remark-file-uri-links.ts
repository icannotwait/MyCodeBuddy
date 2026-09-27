import { isFileName } from "./rehype-relative-file-links"

// Local-file markdown links are otherwise rendered as `… [blocked]`. Three
// distinct sanitize/harden rules cause this, all sidestepped here in the mdast
// layer (before remark-rehype) while keeping the link clickable through the
// existing link-safety + open-file-dialog flow:
//
//   1. `file://` hrefs — rehype-harden hard-codes `file:` in its blocked-
//      protocol list. Rewritten to a bare local path (POSIX `/…`, `/C:/…` for
//      Windows drives, or a `\\server\share` UNC form).
//   2. Bare Windows drive paths (`C:/…`, `C:\…`) — rehype-sanitize reads the
//      leading `C:` as a URL protocol and strips the href, after which harden
//      blocks the now-hrefless `<a>`. Rewritten to `/C:/…` so `C:` is no longer
//      in protocol position (see {@link windowsDrivePathToSafe}).
//   3. A file position at the folder root (`a.ts:12`) — the same misreading,
//      of `a.ts:` this time. Rewritten to `./a.ts:12` (see
//      {@link rootFilePositionToRelative}).
//
// Relative links (`./a.md`, `src/a.md`, `~/a.md`) survive sanitize but not
// harden, which flattens or blocks them; that is handled after sanitize, in
// ./rehype-relative-file-links, where raw HTML anchors are covered too.
//
// Image destinations are handled by remarkLocalImages, which preserves their
// original path until the workspace-confined image reader can resolve it.

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

// A file position at the folder root (`a.ts:12`, `.env:3`, `Makefile:40:2`)
// hits the same wall: with no directory in front, rehype-sanitize reads `a.ts:`
// as a URL protocol and strips the href. `./a.ts:12` puts the colon behind a
// slash, and link-safety splits the line back off before opening. A host with
// a port (`localhost:3000`, `example.com:8080`, `10.0.0.1:80`) has no file
// name in front of the colon and keeps its href.
const ROOT_FILE_POSITION = /^([^\s/\\?#:]+):\d+(?::\d+)?$/

function rootFilePositionToRelative(url: string): string | null {
  const name = ROOT_FILE_POSITION.exec(url)?.[1]
  return name !== undefined && isFileName(name) ? `./${url}` : null
}

function rewriteLocalLinkUrl(url: string): string | null {
  return (
    fileUriToLocalPath(url) ??
    windowsDriveHrefToHardenSafe(url) ??
    rootFilePositionToRelative(url)
  )
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
