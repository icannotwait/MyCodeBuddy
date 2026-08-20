/**
 * Shared JSON object parse helper used by tool-card structured input paths.
 * Extracted so unit tests can spy on body-path parsing without broken
 * local-object spies.
 *
 * Live ACP events re-parse the same `raw_input` string on every token. Short
 * strings share a byte-weighted global LRU; payloads above
 * `PARSE_CACHE_MAX_ENTRY_CHARS` bind only to the live tool object via
 * `parseJsonForOwner` so Write/Edit hitch strings are not retained globally.
 */

export const PARSE_CACHE_MAX = 256
/** Skip the global LRU for payloads larger than this; they are the Write/Edit hitch. */
export const PARSE_CACHE_MAX_ENTRY_CHARS = 64 * 1024
/** Total cached key weight (`length * 2`). Evict LRU until under this budget. */
export const PARSE_CACHE_MAX_CHARS = 256 * 1024
const MISS = Symbol("json-parse-miss")
const parseCache = new Map<string, unknown | typeof MISS>()
let parseCacheChars = 0

type OwnerParseEntry = {
  input: string
  value: unknown | typeof MISS
}

const ownerParseCache = new WeakMap<object, OwnerParseEntry>()

function keyWeight(key: string): number {
  return key.length * 2
}

function asJsonObject(v: unknown): Record<string, unknown> | null {
  return typeof v === "object" && v !== null && !Array.isArray(v)
    ? (v as Record<string, unknown>)
    : null
}

function evictOldest(): void {
  const oldest = parseCache.keys().next().value
  if (oldest === undefined) return
  parseCache.delete(oldest)
  parseCacheChars = Math.max(0, parseCacheChars - keyWeight(oldest))
}

function rememberParse(key: string, value: unknown | typeof MISS): void {
  if (key.length > PARSE_CACHE_MAX_ENTRY_CHARS) {
    return
  }
  if (parseCache.has(key)) {
    parseCache.set(key, value)
    return
  }
  const weight = keyWeight(key)
  while (
    (parseCache.size >= PARSE_CACHE_MAX ||
      parseCacheChars + weight > PARSE_CACHE_MAX_CHARS) &&
    parseCache.size > 0
  ) {
    evictOldest()
  }
  parseCache.set(key, value)
  parseCacheChars += weight
}

function touch(key: string, value: unknown | typeof MISS): void {
  parseCache.delete(key)
  parseCache.set(key, value)
}

/** Parse JSON with a small LRU. Returns `undefined` when `JSON.parse` throws. */
export function parseJsonCached(input: string): unknown | undefined {
  if (input.length > PARSE_CACHE_MAX_ENTRY_CHARS) {
    try {
      return JSON.parse(input) as unknown
    } catch {
      return undefined
    }
  }
  const hit = parseCache.get(input)
  if (hit === MISS) {
    touch(input, MISS)
    return undefined
  }
  if (hit !== undefined) {
    touch(input, hit)
    return hit
  }
  try {
    const parsed: unknown = JSON.parse(input)
    rememberParse(input, parsed)
    return parsed
  } catch {
    rememberParse(input, MISS)
    return undefined
  }
}

export function parseJsonForOwner(
  owner: object,
  input: string
): unknown | undefined {
  if (input.length > PARSE_CACHE_MAX_ENTRY_CHARS) {
    const cached = ownerParseCache.get(owner)
    if (cached && cached.input === input) {
      return cached.value === MISS ? undefined : cached.value
    }
    try {
      const parsed: unknown = JSON.parse(input)
      ownerParseCache.set(owner, { input, value: parsed })
      return parsed
    } catch {
      ownerParseCache.set(owner, { input, value: MISS })
      return undefined
    }
  }
  return parseJsonCached(input)
}

/** Try JSON.parse; return a plain object or null on failure / non-objects. */
export function tryParseJson(s: string): Record<string, unknown> | null {
  return asJsonObject(parseJsonCached(s))
}

export function tryParseJsonForOwner(
  owner: object,
  input: string
): Record<string, unknown> | null {
  return asJsonObject(parseJsonForOwner(owner, input))
}

export function resetJsonParseCacheForTests(): void {
  parseCache.clear()
  parseCacheChars = 0
}
