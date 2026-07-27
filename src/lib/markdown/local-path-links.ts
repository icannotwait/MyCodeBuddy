export type LocalPathKind = "windows-drive" | "posix" | "relative"

export interface LocalPathMatch {
  start: number
  end: number
  label: string
  path: string
  locationSuffix: string | null
  kind: LocalPathKind
}

const WINDOWS_ABSOLUTE = /^[a-zA-Z]:[\\/]/
/** URI scheme shape (RFC 3986-ish). Windows drive `C:/` is excluded separately. */
const URI_SCHEME = /^[a-zA-Z][a-zA-Z0-9+.-]*:/
const LOCATION_SUFFIX = /(#L\d+(?:-L?\d+)?|:\d+(?::\d+)?)$/i
const ROOT_FILE_WITH_EXTENSION = /^\.[^./\\]+$|^[^./\\]+\.[^./\\]+$/
// POSIX paths in chat almost always carry at least one ASCII letter or digit
// (e.g. /Users, /tmp, file.ts). Pure CJK multi-segment strings such as
// "进度/耗时/工具统计" are category labels, not filesystem paths — reject them
// so the leading CJK boundary does not turn the rest into a file badge.
const HAS_ASCII_ALNUM = /[a-zA-Z0-9]/
const START_BLOCKER = /[a-zA-Z0-9_./\\:@~%+#?&=$-]/
const UNQUOTED_TERMINATOR = /[\s`"'<>*|,;!，。；：！？、]/
const SIMPLE_TRAILING = new Set([
  ",",
  ".",
  ";",
  ":",
  "!",
  "?",
  "，",
  "。",
  "；",
  "：",
  "！",
  "？",
  "、",
])
const OPENING_BRACKETS: Record<string, string> = {
  "(": ")",
  "[": "]",
  "{": "}",
  "（": "）",
  "【": "】",
  "「": "」",
  "『": "』",
}
const CLOSING_BRACKETS = new Set(Object.values(OPENING_BRACKETS))

/** Exact whitelist from design — code / config / docs. */
const EXTENSION_WHITELIST = new Set([
  "ts",
  "tsx",
  "js",
  "jsx",
  "mjs",
  "cjs",
  "json",
  "jsonc",
  "md",
  "mdx",
  "txt",
  "rs",
  "go",
  "py",
  "java",
  "kt",
  "cs",
  "cpp",
  "cc",
  "c",
  "h",
  "hpp",
  "css",
  "scss",
  "less",
  "html",
  "htm",
  "xml",
  "yml",
  "yaml",
  "toml",
  "ini",
  "sh",
  "bash",
  "zsh",
  "ps1",
  "bat",
  "cmd",
  "sql",
  "graphql",
  "gql",
  "proto",
  "vue",
  "svelte",
  "astro",
  "swift",
  "rb",
  "php",
  "r",
  "lua",
  "zig",
  "dart",
  "gradle",
  "properties",
  "env",
  "lock",
  "cmake",
  "wasm",
  "map",
  "ipynb",
  "csv",
  // media / archives / office
  "pdf",
  "png",
  "jpg",
  "jpeg",
  "svg",
  "webp",
  "gif",
  "ico",
  "bmp",
  "avif",
  "mp4",
  "webm",
  "mov",
  "mp3",
  "wav",
  "ogg",
  "flac",
  "zip",
  "tar",
  "gz",
  "tgz",
  "7z",
  "rar",
  "docx",
  "xlsx",
  "pptx",
])

/** Special basenames — matched case-insensitively after ASCII fold. */
const SPECIAL_BASENAMES = new Set([
  "dockerfile",
  "makefile",
  "cmakelists.txt",
  ".gitignore",
  ".env",
  ".env.local",
  ".editorconfig",
  ".npmrc",
  ".eslintrc",
  ".prettierrc",
])

function isAsciiLetterCode(code: number): boolean {
  return (code >= 65 && code <= 90) || (code >= 97 && code <= 122)
}

function isAsciiDigitCode(code: number): boolean {
  return code >= 48 && code <= 57
}

function isAbsoluteCandidateStart(text: string, index: number): boolean {
  const first = text.charCodeAt(index)
  if (
    isAsciiLetterCode(first) &&
    text[index + 1] === ":" &&
    (text[index + 2] === "/" || text[index + 2] === "\\")
  ) {
    return true
  }
  return text[index] === "/" && text[index + 1] !== "/"
}

function isRelativeCandidateStart(text: string, index: number): boolean {
  const c = text[index]
  // Explicit ./ or .\
  if (c === "." && (text[index + 1] === "/" || text[index + 1] === "\\")) {
    return true
  }
  // Explicit ../ or ..\
  if (
    c === "." &&
    text[index + 1] === "." &&
    (text[index + 2] === "/" || text[index + 2] === "\\")
  ) {
    return true
  }
  // Dot-prefixed bare directory/file (.github/…); not explicit-relative.
  if (
    c === "." &&
    text[index + 1] !== undefined &&
    text[index + 1] !== "." &&
    text[index + 1] !== "/" &&
    text[index + 1] !== "\\"
  ) {
    return true
  }
  const code = text.charCodeAt(index)
  return isAsciiLetterCode(code) || isAsciiDigitCode(code)
}

function isCandidateStart(text: string, index: number): boolean {
  return (
    isAbsoluteCandidateStart(text, index) ||
    isRelativeCandidateStart(text, index)
  )
}

function hasStartBoundary(text: string, index: number): boolean {
  return index === 0 || !START_BLOCKER.test(text[index - 1])
}

function trimUnquotedCandidate(value: string): string {
  let end = value.length
  while (end > 0 && SIMPLE_TRAILING.has(value[end - 1])) end -= 1
  return value.slice(0, end)
}

function stripLocationSuffix(path: string): string {
  const suffixMatch = path.match(LOCATION_SUFFIX)
  if (!suffixMatch?.[1]) return path
  return path.slice(0, path.length - suffixMatch[1].length)
}

function hasEmptySegment(normalizedSlashPath: string): boolean {
  return normalizedSlashPath.split("/").some((segment) => segment === "")
}

/**
 * True for scheme-bearing tokens (`mailto:…`, `file:…`, `https:…`, `custom:…`).
 * Windows drive letters (`C:/`, `D:\`) are not treated as URI schemes.
 */
function hasUriScheme(path: string): boolean {
  if (!path) return false
  // Drive absolute first — single-letter + : + separator is not a scheme here.
  if (WINDOWS_ABSOLUTE.test(path)) return false
  const normalized = path.replace(/\\/g, "/")
  if (/^[a-zA-Z]:\//.test(normalized)) return false
  return URI_SCHEME.test(path)
}

function isExplicitRelativeForm(path: string): boolean {
  const normalized = path.replace(/\\/g, "/")
  return normalized.startsWith("./") || normalized.startsWith("../")
}

function isBareRelativeForm(path: string): boolean {
  if (!path || /[\r\n]/.test(path)) return false
  if (path.startsWith("/") || path.startsWith("\\")) return false
  if (path.startsWith("~")) return false
  if (path.startsWith("@")) return false
  if (WINDOWS_ABSOLUTE.test(path)) return false
  if (hasUriScheme(path)) return false
  if (!/[\\/]/.test(path)) return false
  return true
}

function isRelativeForm(path: string): boolean {
  return isExplicitRelativeForm(path) || isBareRelativeForm(path)
}

function isSpecialBasename(basename: string): boolean {
  return SPECIAL_BASENAMES.has(basename.toLowerCase())
}

function extensionOf(basename: string): string | null {
  const dot = basename.lastIndexOf(".")
  if (dot <= 0) return null
  const ext = basename.slice(dot + 1)
  return ext || null
}

function hasWhitelistedExtensionOrSpecial(basename: string): boolean {
  if (isSpecialBasename(basename)) return true
  const ext = extensionOf(basename)
  if (!ext) return false
  return EXTENSION_WHITELIST.has(ext.toLowerCase())
}

/**
 * Shared relative gates used by the prose autolink gate and bare openability.
 * Caller must strip location suffixes and ensure relative form.
 */
function passesSharedRelativeGates(path: string): boolean {
  if (!path || path.includes("$") || /[\r\n]/.test(path)) return false
  if (hasUriScheme(path)) return false
  if (!HAS_ASCII_ALNUM.test(path)) return false

  const normalized = path.replace(/\\/g, "/")
  if (hasEmptySegment(normalized)) return false

  const isExplicit = isExplicitRelativeForm(path)
  if (!isExplicit) {
    const firstSegment = normalized.split("/")[0] ?? ""
    // Hostname-like first segment (bare only): contains `.` and not dotfile/dir.
    if (firstSegment.includes(".") && !firstSegment.startsWith(".")) {
      return false
    }
  }

  const segments = normalized.split("/")
  const basename = segments[segments.length - 1] ?? ""
  if (!basename) return false
  return hasWhitelistedExtensionOrSpecial(basename)
}

/**
 * Autolink confidence for prose scanner (kind === "relative").
 * Applies extension/special basename to ALL relative forms (bare and
 * explicit ./ ../).
 */
export function passesRelativeAutolinkGate(path: string): boolean {
  if (!path) return false
  const stripped = stripLocationSuffix(path)
  if (!isRelativeForm(stripped)) return false
  return passesSharedRelativeGates(stripped)
}

/**
 * Bare-relative openability (used by isLocalPathLike).
 * Requires separator + not absolute + extension/special basename +
 * hostname-first-segment + $ reject + ASCII alnum + not @/~-prefixed.
 * Does NOT apply to paths that already start with ./ or ../.
 */
export function isBareRelativeWorkspacePathLike(path: string): boolean {
  if (!path) return false
  const stripped = stripLocationSuffix(path)
  if (!isBareRelativeForm(stripped)) return false
  return passesSharedRelativeGates(stripped)
}

/**
 * Openability: can this string be treated as a local file target for click/icon?
 * UNC check must run before slash normalize (raw `\\` vs protocol-relative `//`).
 */
export function isLocalPathLike(path: string): boolean {
  if (!path) return false
  if (path.startsWith("\\\\")) return true // raw UNC
  if (path.startsWith("//")) return false // protocol-relative
  if (path.startsWith("/")) return true // raw POSIX only
  if (path.startsWith("~/")) return true // raw home only
  const normalized = path.replace(/\\/g, "/")
  if (normalized.startsWith("./") || normalized.startsWith("../")) return true
  if (/^[a-zA-Z]:\//.test(normalized)) return true
  return isBareRelativeWorkspacePathLike(path)
}

function classifyAbsolute(path: string): LocalPathKind | null {
  if (!path || /[\r\n]/.test(path)) return null
  if (WINDOWS_ABSOLUTE.test(path)) return "windows-drive"
  if (!path.startsWith("/") || path.startsWith("//")) return null
  const body = path.slice(1)
  if (!body) return null
  if (!HAS_ASCII_ALNUM.test(body)) return null
  if (body.includes("/")) return "posix"
  return ROOT_FILE_WITH_EXTENSION.test(body) ? "posix" : null
}

function classifyPath(path: string): LocalPathKind | null {
  if (!path || /[\r\n]/.test(path)) return null
  if (path.includes("$")) return null

  // Absolute (including Windows drive) before any scheme / relative checks.
  const absoluteKind = classifyAbsolute(path)
  if (absoluteKind) return absoluteKind

  // Whole-token reject of scheme-bearing tokens (mailto:, file:, custom:, …).
  if (hasUriScheme(path)) return null

  if (isRelativeForm(path) && passesRelativeAutolinkGate(path)) {
    return "relative"
  }
  return null
}

function parseCandidate(
  label: string,
  start: number,
  end: number
): LocalPathMatch | null {
  if (label.includes("$")) return null
  const suffixMatch = label.match(LOCATION_SUFFIX)
  const locationSuffix = suffixMatch?.[1] ?? null
  const path = locationSuffix
    ? label.slice(0, label.length - locationSuffix.length)
    : label
  const kind = classifyPath(path)
  if (!kind) return null
  return { start, end, label, path, locationSuffix, kind }
}

function findUnquotedEnd(text: string, start: number): number {
  const expectedClosers: string[] = []
  let end = start
  while (end < text.length) {
    const current = text[end]
    if (UNQUOTED_TERMINATOR.test(current)) break
    const expectedCloser = OPENING_BRACKETS[current]
    if (expectedCloser) {
      expectedClosers.push(expectedCloser)
    } else if (CLOSING_BRACKETS.has(current)) {
      if (expectedClosers[expectedClosers.length - 1] !== current) break
      expectedClosers.pop()
    }
    end += 1
  }
  return end
}

function isEscapedQuote(text: string, index: number): boolean {
  let slashCount = 0
  for (
    let cursor = index - 1;
    cursor >= 0 && text[cursor] === "\\";
    cursor -= 1
  ) {
    slashCount += 1
  }
  return slashCount % 2 === 1
}

function findQuotedClose(
  text: string,
  start: number,
  quote: string
): { end: number; containsEscapedQuote: boolean } | null {
  let containsEscapedQuote = false
  for (let end = start; end < text.length; end += 1) {
    if (text[end] !== quote) continue
    if (isEscapedQuote(text, end)) {
      containsEscapedQuote = true
      continue
    }
    return { end, containsEscapedQuote }
  }
  return null
}

export function findLocalPathRanges(text: string): LocalPathMatch[] {
  const matches: LocalPathMatch[] = []
  let index = 0

  while (index < text.length) {
    const current = text[index]
    if (current === '"' || current === "'") {
      if (!isCandidateStart(text, index + 1)) {
        index += 1
        continue
      }
      if (isEscapedQuote(text, index)) {
        const escapedClose = text.indexOf(current, index + 1)
        if (escapedClose < 0) break
        index = escapedClose + 1
        continue
      }
      const close = findQuotedClose(text, index + 1, current)
      if (!close) break
      const label = text.slice(index + 1, close.end)
      const containsNestedQuote = label.includes(current === '"' ? "'" : '"')
      if (!close.containsEscapedQuote && !containsNestedQuote) {
        const match = parseCandidate(label, index + 1, close.end)
        if (match) matches.push(match)
      }
      index = close.end + 1
      continue
    }

    if (!isCandidateStart(text, index) || !hasStartBoundary(text, index)) {
      index += 1
      continue
    }

    const scannedEnd = findUnquotedEnd(text, index)
    const label = trimUnquotedCandidate(text.slice(index, scannedEnd))
    const end = index + label.length
    const match = parseCandidate(label, index, end)
    if (match) matches.push(match)
    index = Math.max(scannedEnd, index + 1)
  }

  return matches
}

/** @deprecated Use findLocalPathRanges. */
export function findAbsoluteLocalPathRanges(text: string): LocalPathMatch[] {
  return findLocalPathRanges(text)
}

function encodePathSegment(segment: string): string {
  return encodeURIComponent(segment)
}

export function toSafeLocalPathHref(match: LocalPathMatch): string | null {
  try {
    if (match.kind === "relative") {
      const normalized = match.path.replace(/\\/g, "/")
      if (hasEmptySegment(normalized)) return null
      const encoded = normalized
        .split("/")
        .map((segment) => encodePathSegment(segment))
        .join("/")
      // Relative href must never gain a leading `/`.
      return `${encoded}${match.locationSuffix ?? ""}`
    }

    const normalized =
      match.kind === "windows-drive"
        ? `/${match.path.replace(/\\/g, "/")}`
        : match.path
    const encoded = normalized
      .split("/")
      .map((segment, index) => {
        if (
          match.kind === "windows-drive" &&
          index === 1 &&
          /^[a-zA-Z]:$/.test(segment)
        ) {
          return segment
        }
        return encodePathSegment(segment)
      })
      .join("/")
    return `${encoded}${match.locationSuffix ?? ""}`
  } catch {
    return null
  }
}
