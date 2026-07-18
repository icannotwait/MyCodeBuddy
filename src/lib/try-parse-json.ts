/**
 * Shared JSON object parse helper used by tool-card structured input paths.
 * Extracted so unit tests can spy on body-path parsing without broken
 * local-object spies.
 */

/** Try JSON.parse; return a plain object or null on failure / non-objects. */
export function tryParseJson(s: string): Record<string, unknown> | null {
  try {
    const v = JSON.parse(s)
    return typeof v === "object" && v !== null && !Array.isArray(v) ? v : null
  } catch {
    return null
  }
}
