export const REFERENCE_DISPLAY_BASELINE_TEXT =
  "2026-07-06-simple-packaging-storage-ballistic-throw-design.md"

export const REFERENCE_DISPLAY_MAX_CHARS = Array.from(
  REFERENCE_DISPLAY_BASELINE_TEXT
).length

const REFERENCE_DISPLAY_ELLIPSIS = "..."

export function middleTruncateReferenceText(
  text: string,
  maxChars = REFERENCE_DISPLAY_MAX_CHARS
): string {
  const chars = Array.from(text)
  if (chars.length <= maxChars) return text

  const edgeChars = Math.max(
    1,
    Math.floor((maxChars - REFERENCE_DISPLAY_ELLIPSIS.length) / 2)
  )
  return [
    chars.slice(0, edgeChars).join(""),
    REFERENCE_DISPLAY_ELLIPSIS,
    chars.slice(-edgeChars).join(""),
  ].join("")
}
