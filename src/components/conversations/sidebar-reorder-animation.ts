import {
  sidebarRowKey,
  type SidebarBucketKey,
  type SidebarRow,
} from "./sidebar-conversation-grouping"

export interface SidebarRootOrderSnapshot {
  structuralRowKeys: readonly string[]
  rootsByBucket: ReadonlyMap<SidebarBucketKey, readonly number[]>
  blockRowKeysByRoot: ReadonlyMap<number, readonly string[]>
  bucketByRoot: ReadonlyMap<number, SidebarBucketKey>
}

export interface SidebarActivityReorder {
  conversationId: number
  bucketKey: SidebarBucketKey
  previousIndex: number
  nextIndex: number
}

export interface SidebarMeasuredRow {
  key: string
  rootId: number | null
  top: number
  bottom: number
}

function sameReadonlyStringArray(
  a: readonly string[],
  b: readonly string[]
): boolean {
  if (a.length !== b.length) return false
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return false
  }
  return true
}

function sameNumberSet(a: readonly number[], b: readonly number[]): boolean {
  if (a.length !== b.length) return false
  const set = new Set(a)
  for (const value of b) {
    if (!set.has(value)) return false
  }
  return true
}

function sameBucketRootMembership(
  a: ReadonlyMap<SidebarBucketKey, readonly number[]>,
  b: ReadonlyMap<SidebarBucketKey, readonly number[]>
): boolean {
  if (a.size !== b.size) return false
  for (const [bucket, rootsA] of a) {
    const rootsB = b.get(bucket)
    if (!rootsB || !sameNumberSet(rootsA, rootsB)) return false
  }
  return true
}

function sameBucketByRoot(
  a: ReadonlyMap<number, SidebarBucketKey>,
  b: ReadonlyMap<number, SidebarBucketKey>
): boolean {
  if (a.size !== b.size) return false
  for (const [rootId, bucket] of a) {
    if (b.get(rootId) !== bucket) return false
  }
  return true
}

function sameBlockRowKeysByRoot(
  a: ReadonlyMap<number, readonly string[]>,
  b: ReadonlyMap<number, readonly string[]>
): boolean {
  if (a.size !== b.size) return false
  for (const [rootId, keysA] of a) {
    const keysB = b.get(rootId)
    if (!keysB || !sameReadonlyStringArray(keysA, keysB)) return false
  }
  return true
}

/**
 * Build a pure structural snapshot of root-block order for animation eligibility.
 * Structural keys are unowned rows only; owned conversation/loading rows live in
 * root blocks. Bucket identity always comes from the row's `bucketKey`.
 */
export function buildSidebarRootOrderSnapshot(
  rows: readonly SidebarRow[]
): SidebarRootOrderSnapshot {
  const structuralRowKeys: string[] = []
  const rootsByBucket = new Map<SidebarBucketKey, number[]>()
  const blockRowKeysByRoot = new Map<number, string[]>()
  const bucketByRoot = new Map<number, SidebarBucketKey>()

  for (const row of rows) {
    if (row.kind === "conversation" || row.kind === "subsession-loading") {
      const key = sidebarRowKey(row)
      const { rootId, bucketKey } = row

      let block = blockRowKeysByRoot.get(rootId)
      if (!block) {
        block = []
        blockRowKeysByRoot.set(rootId, block)
      }
      block.push(key)

      if (row.kind === "conversation" && row.depth === 0) {
        if (!bucketByRoot.has(rootId)) {
          bucketByRoot.set(rootId, bucketKey)
          let roots = rootsByBucket.get(bucketKey)
          if (!roots) {
            roots = []
            rootsByBucket.set(bucketKey, roots)
          }
          roots.push(rootId)
        }
      }
      continue
    }

    structuralRowKeys.push(sidebarRowKey(row))
  }

  return {
    structuralRowKeys,
    rootsByBucket,
    blockRowKeysByRoot,
    bucketByRoot,
  }
}

/**
 * Detect an eligible upward same-bucket root permutation for the activity root.
 * Returns null on any structural, membership, block, or non-upward change.
 */
export function detectSidebarActivityReorder(
  before: SidebarRootOrderSnapshot,
  after: SidebarRootOrderSnapshot,
  conversationId: number
): SidebarActivityReorder | null {
  if (
    !sameReadonlyStringArray(before.structuralRowKeys, after.structuralRowKeys)
  ) {
    return null
  }
  if (!sameBucketByRoot(before.bucketByRoot, after.bucketByRoot)) {
    return null
  }
  if (!sameBucketRootMembership(before.rootsByBucket, after.rootsByBucket)) {
    return null
  }
  if (
    !sameBlockRowKeysByRoot(before.blockRowKeysByRoot, after.blockRowKeysByRoot)
  ) {
    return null
  }

  const bucketKey = before.bucketByRoot.get(conversationId)
  if (bucketKey === undefined) return null
  if (after.bucketByRoot.get(conversationId) !== bucketKey) return null

  const beforeRoots = before.rootsByBucket.get(bucketKey)
  const afterRoots = after.rootsByBucket.get(bucketKey)
  if (!beforeRoots || !afterRoots) return null

  const previousIndex = beforeRoots.indexOf(conversationId)
  const nextIndex = afterRoots.indexOf(conversationId)
  if (previousIndex < 0 || nextIndex < 0) return null
  if (nextIndex >= previousIndex) return null

  return {
    conversationId,
    bucketKey,
    previousIndex,
    nextIndex,
  }
}

/**
 * Choose the first fully-visible surviving measured row outside the promoted
 * root block, ordered by ascending `top`. Fully visible uses inclusive edges.
 */
export function selectSidebarAnchor(
  before: ReadonlyMap<string, SidebarMeasuredRow>,
  survivingKeys: ReadonlySet<string>,
  viewportTop: number,
  viewportBottom: number,
  promotedRootId: number
): SidebarMeasuredRow | null {
  const ordered = [...before.values()].sort((a, b) => a.top - b.top)

  for (const row of ordered) {
    if (!survivingKeys.has(row.key)) continue
    if (row.rootId === promotedRootId) continue
    if (row.top < viewportTop || row.bottom > viewportBottom) continue
    return row
  }

  return null
}

export const sidebarAnchorScrollDelta = (beforeTop: number, afterTop: number) =>
  afterTop - beforeTop

export const sidebarFlipDeltaY = (beforeTop: number, afterTop: number) =>
  beforeTop - afterTop
