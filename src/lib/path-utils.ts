export function joinFsPath(basePath: string, relPath: string): string {
  if (!relPath) return basePath
  const separator = basePath.includes("\\") ? "\\" : "/"
  const normalizedRel = relPath.replace(/[\\/]/g, separator)
  if (basePath.endsWith("/") || basePath.endsWith("\\")) {
    return `${basePath}${normalizedRel}`
  }
  return `${basePath}${separator}${normalizedRel}`
}

/**
 * Return the parent directory of an OS path, using whichever separator the
 * path itself uses. Returns `null` when there is no meaningful parent —
 * i.e. the path is already a root (`/`, `C:\`, `\\server\share`) or empty.
 *
 * The server file browser navigates Windows and POSIX paths transparently
 * depending on which OS the remote codeg-server runs on, so a single
 * `split("/")` would silently break Windows roots like `C:\Users\foo`.
 *
 * UNC paths (`\\server\share\...`) are treated specially: the share root
 * itself has no navigable parent — `\\server` is not a real location on
 * Windows. A naïve pop would expose the host as if it were a directory,
 * letting the UI navigate into a path the OS can't open.
 */
export function parentFsPath(path: string): string | null {
  if (!path) return null
  const usesBackslash = path.includes("\\")
  const separator = usesBackslash ? "\\" : "/"
  // Detect a UNC prefix (`\\host\...` or `//host/...`). The third
  // character must be a non-separator so the regex doesn't match
  // pathological inputs like `\\\\`.
  const isUnc = /^[\\/][\\/][^\\/]/.test(path)
  // Strip trailing separators, but never collapse the leading separator(s)
  // of a POSIX root or a UNC prefix.
  const trimmed = path.replace(/[/\\]+$/, "")
  if (!trimmed) {
    // The path was nothing but separators: `/`, `\\`, ... — already root.
    return null
  }
  // Windows drive root: `C:` or `C:\`. After trimming trailing separators
  // we land on `C:` which has no parent.
  if (/^[A-Za-z]:$/.test(trimmed)) return null
  const parts = trimmed.split(/[\\/]/)

  if (isUnc) {
    // `\\server\share\folder` splits to ["", "", "server", "share",
    // "folder"]. The first two empties are the UNC prefix; "server"
    // and "share" are the host and share components — both mandatory.
    // Length ≤ 4 means we're at or above the share root, where the
    // only navigable parent doesn't exist on Windows.
    if (parts.length <= 4) return null
    parts.pop()
    return parts.join(separator)
  }

  if (parts.length <= 1) {
    // POSIX root degenerate case (`foo` with no leading slash) — no
    // meaningful parent we can navigate to.
    return null
  }
  parts.pop()
  const parent = parts.join(separator)
  if (!parent) {
    // Joined to empty means we were one segment below the root. Return
    // the explicit root so the UI navigates to `/` rather than `""`.
    return separator
  }
  // Windows drive root needs a trailing separator (`C:\`, not `C:`) for
  // path APIs and for visual clarity.
  if (/^[A-Za-z]:$/.test(parent)) return `${parent}${separator}`
  return parent
}
