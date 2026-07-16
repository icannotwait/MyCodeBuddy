/**
 * Deterministic weighted LRU cache with entry-count and total-weight budgets.
 * Recency is Map insertion order: `get` reinserts; `peek` does not.
 */

export class WeightedLruCache<K, V> {
  private readonly entries = new Map<K, { value: V; weight: number }>()
  private weight = 0

  constructor(
    private readonly options: {
      maxEntries: number
      maxWeight: number
      weightOf(value: V, key: K): number
    }
  ) {}

  get size(): number {
    return this.entries.size
  }

  get totalWeight(): number {
    return this.weight
  }

  get(key: K): V | undefined {
    const entry = this.entries.get(key)
    if (!entry) return undefined
    this.entries.delete(key)
    this.entries.set(key, entry)
    return entry.value
  }

  peek(key: K): V | undefined {
    return this.entries.get(key)?.value
  }

  has(key: K): boolean {
    return this.entries.has(key)
  }

  keys(): IterableIterator<K> {
    return this.entries.keys()
  }

  take(key: K): V | undefined {
    const entry = this.entries.get(key)
    if (!entry) return undefined
    this.delete(key)
    return entry.value
  }

  set(key: K, value: V): boolean {
    const weight = Math.max(0, this.options.weightOf(value, key))
    if (weight > this.options.maxWeight) return false
    this.delete(key)
    this.entries.set(key, { value, weight })
    this.weight += weight
    while (
      this.entries.size > this.options.maxEntries ||
      this.weight > this.options.maxWeight
    ) {
      const oldest = this.entries.keys().next().value as K | undefined
      if (oldest === undefined) break
      this.delete(oldest)
    }
    return true
  }

  delete(key: K): boolean {
    const entry = this.entries.get(key)
    if (!entry) return false
    this.entries.delete(key)
    this.weight -= entry.weight
    return true
  }

  clear(): void {
    this.entries.clear()
    this.weight = 0
  }

  /** Content-free entry/byte snapshot for soak and diagnostics. */
  stats(): { entries: number; bytes: number } {
    return { entries: this.entries.size, bytes: this.weight }
  }
}
