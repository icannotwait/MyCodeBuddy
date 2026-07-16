import { describe, expect, it } from "vitest"
import { WeightedLruCache } from "./weighted-lru"

describe("WeightedLruCache", () => {
  it("evicts least-recent entries by entry and byte budgets", () => {
    const cache = new WeightedLruCache<string, string>({
      maxEntries: 2,
      maxWeight: 6,
      weightOf: (value) => value.length,
    })
    expect(cache.set("a", "aa")).toBe(true)
    expect(cache.set("b", "bb")).toBe(true)
    expect(cache.get("a")).toBe("aa")
    expect(cache.set("c", "cccc")).toBe(true)
    expect(cache.has("a")).toBe(true)
    expect(cache.has("b")).toBe(false)
    expect(cache.totalWeight).toBe(6)
  })

  it("rejects an entry larger than the entire budget", () => {
    const cache = new WeightedLruCache<string, string>({
      maxEntries: 4,
      maxWeight: 3,
      weightOf: (value) => value.length,
    })
    expect(cache.set("oversize", "1234")).toBe(false)
    expect(cache.size).toBe(0)
  })

  it("peek does not change recency and take removes once", () => {
    const cache = new WeightedLruCache<string, string>({
      maxEntries: 2,
      maxWeight: 10,
      weightOf: (value) => value.length,
    })
    cache.set("a", "aa")
    cache.set("b", "bb")
    expect(cache.peek("a")).toBe("aa")
    // peek left "a" oldest → setting "c" evicts "a"
    expect(cache.set("c", "cc")).toBe(true)
    expect(cache.has("a")).toBe(false)
    expect(cache.take("b")).toBe("bb")
    expect(cache.has("b")).toBe(false)
    expect(cache.size).toBe(1)
  })

  it("stats and clear return content-free zeros", () => {
    const cache = new WeightedLruCache<string, string>({
      maxEntries: 4,
      maxWeight: 100,
      weightOf: (value) => value.length,
    })
    cache.set("a", "hello")
    expect(cache.stats()).toEqual({ entries: 1, bytes: 5 })
    cache.clear()
    expect(cache.stats()).toEqual({ entries: 0, bytes: 0 })
  })
})
