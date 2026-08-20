import { afterEach, describe, expect, it, vi } from "vitest"
import {
  PARSE_CACHE_MAX,
  PARSE_CACHE_MAX_CHARS,
  PARSE_CACHE_MAX_ENTRY_CHARS,
  parseJsonCached,
  parseJsonForOwner,
  resetJsonParseCacheForTests,
  tryParseJson,
} from "@/lib/try-parse-json"

afterEach(() => {
  resetJsonParseCacheForTests()
  vi.restoreAllMocks()
})

function hugeWritePayload(fill: string): string {
  return `{"content":"${fill.repeat(128 * 1024)}"}`
}

describe("tryParseJson / parseJsonCached", () => {
  it("returns a plain object and caches the parse", () => {
    const spy = vi.spyOn(JSON, "parse")
    const input = '{"a":1}'
    expect(tryParseJson(input)).toEqual({ a: 1 })
    expect(tryParseJson(input)).toEqual({ a: 1 })
    expect(spy).toHaveBeenCalledTimes(1)
  })

  it("returns null for arrays, primitives, and invalid JSON", () => {
    expect(tryParseJson("[1]")).toBeNull()
    expect(tryParseJson("5")).toBeNull()
    expect(tryParseJson("not json")).toBeNull()
    expect(parseJsonCached("not json")).toBeUndefined()
    expect(parseJsonCached("not json")).toBeUndefined()
  })

  it("promotes hits so a hot Write payload survives unique small parses", () => {
    const spy = vi.spyOn(JSON, "parse")
    const write = `{"content":"${"x".repeat(64)}"}`
    expect(parseJsonCached(write)).toEqual({ content: "x".repeat(64) })
    expect(spy).toHaveBeenCalledTimes(1)

    for (let i = 0; i < PARSE_CACHE_MAX - 1; i++) {
      parseJsonCached(`{"i":${i}}`)
      parseJsonCached(write)
    }
    spy.mockClear()
    expect(parseJsonCached(write)).toEqual({ content: "x".repeat(64) })
    expect(spy).not.toHaveBeenCalled()
  })

  it("weights the global LRU by a conservative length * 2 byte estimate", () => {
    // Each ~32 KiB key weighs ~64 KiB at length * 2. Five of them exceed
    // PARSE_CACHE_MAX_CHARS (256 KiB) and must evict the oldest. Char-counting
    // would keep all five (~160 KiB of keys) and this would not reparse.
    const keys = Array.from(
      { length: 5 },
      (_, i) => `{"k":"${"a".repeat(32 * 1024)}","i":${i}}`
    )
    for (const key of keys) parseJsonCached(key)
    const spy = vi.spyOn(JSON, "parse")
    expect(parseJsonCached(keys[0]!)).toEqual({
      k: "a".repeat(32 * 1024),
      i: 0,
    })
    expect(spy).toHaveBeenCalledTimes(1)
    expect(PARSE_CACHE_MAX_CHARS).toBe(256 * 1024)
  })

  it("does not put payloads larger than the per-entry budget in the global LRU", () => {
    const huge = `{"content":"${"x".repeat(PARSE_CACHE_MAX_ENTRY_CHARS)}"}`
    const spy = vi.spyOn(JSON, "parse")
    expect(parseJsonCached(huge)).toEqual({
      content: "x".repeat(PARSE_CACHE_MAX_ENTRY_CHARS),
    })
    expect(parseJsonCached(huge)).toEqual({
      content: "x".repeat(PARSE_CACHE_MAX_ENTRY_CHARS),
    })
    expect(spy).toHaveBeenCalledTimes(2)
  })
})

describe("parseJsonForOwner", () => {
  it("parses a large payload only once for the same owner", () => {
    const owner = {}
    const huge = hugeWritePayload("x")
    const spy = vi.spyOn(JSON, "parse")
    expect(parseJsonForOwner(owner, huge)).toBeDefined()
    expect(parseJsonForOwner(owner, huge)).toBeDefined()
    expect(spy).toHaveBeenCalledTimes(1)
  })

  it("reparses exactly once when the same owner's input string changes", () => {
    const owner = {}
    const first = hugeWritePayload("x")
    const second = hugeWritePayload("y")
    const spy = vi.spyOn(JSON, "parse")
    expect(parseJsonForOwner(owner, first)).toEqual({
      content: "x".repeat(128 * 1024),
    })
    expect(parseJsonForOwner(owner, second)).toEqual({
      content: "y".repeat(128 * 1024),
    })
    expect(spy).toHaveBeenCalledTimes(2)
    spy.mockClear()
    expect(parseJsonForOwner(owner, second)).toEqual({
      content: "y".repeat(128 * 1024),
    })
    expect(spy).not.toHaveBeenCalled()
  })

  it("does not reuse a parse when the owner is replaced", () => {
    const huge = hugeWritePayload("x")
    const spy = vi.spyOn(JSON, "parse")
    expect(parseJsonForOwner({}, huge)).toBeDefined()
    expect(parseJsonForOwner({}, huge)).toBeDefined()
    expect(spy).toHaveBeenCalledTimes(2)
  })

  it("does not insert large owner-cached payloads into the string-keyed Map", () => {
    const owner = {}
    const huge = hugeWritePayload("x")
    expect(parseJsonForOwner(owner, huge)).toBeDefined()
    const spy = vi.spyOn(JSON, "parse")
    expect(parseJsonCached(huge)).toBeDefined()
    expect(spy).toHaveBeenCalledTimes(1)
  })

  it("still shares the global LRU across owners for short strings", () => {
    const input = '{"a":1}'
    const spy = vi.spyOn(JSON, "parse")
    expect(parseJsonForOwner({}, input)).toEqual({ a: 1 })
    expect(parseJsonForOwner({}, input)).toEqual({ a: 1 })
    expect(spy).toHaveBeenCalledTimes(1)
  })
})
