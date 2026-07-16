export type LocalPathKind = "windows-drive" | "posix"

export interface LocalPathMatch {
  start: number
  end: number
  label: string
  path: string
  locationSuffix: string | null
  kind: LocalPathKind
}

const WINDOWS_ABSOLUTE = /^[a-zA-Z]:[\\/]/
const LOCATION_SUFFIX = /(#L\d+(?:-L?\d+)?|:\d+(?::\d+)?)$/i
const ROOT_FILE_WITH_EXTENSION = /^\.[^./\\]+$|^[^./\\]+\.[^./\\]+$/
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

function isCandidateStart(text: string, index: number): boolean {
  const first = text.charCodeAt(index)
  const isAsciiLetter =
    (first >= 65 && first <= 90) || (first >= 97 && first <= 122)
  if (
    isAsciiLetter &&
    text[index + 1] === ":" &&
    (text[index + 2] === "/" || text[index + 2] === "\\")
  ) {
    return true
  }
  return text[index] === "/" && text[index + 1] !== "/"
}

function hasStartBoundary(text: string, index: number): boolean {
  return index === 0 || !START_BLOCKER.test(text[index - 1])
}

function trimUnquotedCandidate(value: string): string {
  let end = value.length
  while (end > 0 && SIMPLE_TRAILING.has(value[end - 1])) end -= 1
  return value.slice(0, end)
}

function classifyPath(path: string): LocalPathKind | null {
  if (!path || /[\r\n]/.test(path)) return null
  if (WINDOWS_ABSOLUTE.test(path)) return "windows-drive"
  if (!path.startsWith("/") || path.startsWith("//")) return null
  const body = path.slice(1)
  if (!body) return null
  if (body.includes("/")) return "posix"
  return ROOT_FILE_WITH_EXTENSION.test(body) ? "posix" : null
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

export function findAbsoluteLocalPathRanges(text: string): LocalPathMatch[] {
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

function encodePathSegment(segment: string): string {
  return encodeURIComponent(segment)
}

export function toSafeLocalPathHref(match: LocalPathMatch): string | null {
  try {
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
