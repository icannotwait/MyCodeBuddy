import { formatConversationTitle } from "@/lib/conversation-title"
import type {
  DbConversationSummary,
  ReferenceCandidate,
  ReferenceRegexRank,
} from "@/lib/types"
import { registerBackendScopedStoreReset } from "@/stores/backend-scoped-store-reset"

const DEFAULT_CANDIDATE_CAP = 10_000
const DEFAULT_CANDIDATE_BYTE_CAP = 64 * 1024 * 1024
const DEFAULT_REGEX_SNAPSHOT_CAP = 200
const CONVERSATION_TOMBSTONE_CAP = 512
const EVENT_WATERMARK_CAP = 10_000

const textEncoder = new TextEncoder()

export interface ReferenceSearchCacheOptions {
  candidateCap?: number
  candidateByteCap?: number
  regexSnapshotCap?: number
}

export type ReferenceCacheBucketKey =
  | { backend: string; source: "file"; canonicalRoot: string }
  | { backend: string; source: "conversation" }
  | {
      backend: string
      source: "commit"
      canonicalRepo: string
      sourceEpoch: string
    }

export interface CachedCandidate {
  candidate: ReferenceCandidate
  mutationRevision: number
  stale: boolean
}

export interface LiteralCachePreview {
  items: CachedCandidate[]
  truncated: boolean
}

export type ReferenceConversationCacheChange =
  | {
      kind: "upsert"
      backend: string
      uri: string
      summary: DbConversationSummary
      mutationRevision: number | null
    }
  | {
      kind: "status"
      backend: string
      uri: string
      status: string
      mutationRevision: number | null
    }
  | {
      kind: "delete"
      backend: string
      uri: string
    }

export interface RegexRefreshHandle {
  readonly token: number
  readonly controllerId: string
  readonly bucketKey: string
  readonly expression: string
}

type ConversationChangeListener = (
  change: ReferenceConversationCacheChange
) => void

interface ItemEntry {
  candidate: ReferenceCandidate
  mutationRevision: number
  retainedUtf8Bytes: number
  lruTick: number
}

interface RegexRef {
  uri: string
  rank: ReferenceRegexRank
  stale: boolean
  mutationRevision: number
}

interface RegexSnapshot {
  refs: RegexRef[]
  truncated: boolean
  lruTick: number
}

interface ControllerState {
  visible: Map<string, Set<string>>
  selected: { bucketKey: string; uri: string } | null
  activeRegex: Map<string, string>
}

interface ActiveRegexRefresh {
  handle: RegexRefreshHandle
  backend: string
  revisionSnapshot: Map<string, number>
}

function utf8Bytes(value: string): number {
  return textEncoder.encode(value).byteLength
}

function encodeLengthDelimited(parts: readonly string[]): string {
  let out = ""
  for (const part of parts) {
    out += `${part.length}\0${part}`
  }
  return out
}

export function serializeBucketKey(key: ReferenceCacheBucketKey): string {
  if (key.source === "file") {
    return encodeLengthDelimited(["file", key.backend, key.canonicalRoot])
  }
  if (key.source === "conversation") {
    return encodeLengthDelimited(["conversation", key.backend])
  }
  return encodeLengthDelimited([
    "commit",
    key.backend,
    key.canonicalRepo,
    key.sourceEpoch,
  ])
}

export function sessionUri(conversationId: number | string): string {
  return `codeg://session/${conversationId}`
}

export function candidateRetainedUtf8Bytes(
  candidate: ReferenceCandidate
): number {
  let total = 0
  const add = (value: string | null | undefined) => {
    if (value != null) total += utf8Bytes(value)
  }
  add(candidate.source)
  add(candidate.uri)
  add(candidate.id)
  add(candidate.label)
  add(candidate.detail)
  add(candidate.keywords)
  const meta = candidate.metadata
  add(meta.kind)
  if (meta.kind === "file") {
    add(meta.canonicalWorkspaceRoot)
    add(meta.relativePath)
    add(meta.entryKind)
  } else if (meta.kind === "conversation") {
    add(meta.agentType)
    add(meta.status)
    add(meta.branch)
    add(meta.projectName)
    add(meta.projectPath)
  } else {
    add(meta.canonicalRepo)
    add(meta.fullHash)
    add(meta.shortHash)
    add(meta.subject)
    add(meta.message)
    add(meta.author)
    add(meta.authoredAt)
  }
  return total
}

export function candidateSearchFields(candidate: ReferenceCandidate): {
  primary: string[]
  secondary: string[]
} {
  const meta = candidate.metadata
  if (meta.kind === "file") {
    return {
      primary: [candidate.label],
      secondary: [meta.relativePath],
    }
  }
  if (meta.kind === "conversation") {
    return {
      primary: [candidate.label],
      secondary: [
        candidate.id,
        meta.agentType,
        meta.status,
        meta.branch ?? "",
        meta.projectName,
        meta.projectPath,
      ],
    }
  }
  return {
    primary: [meta.shortHash, meta.fullHash, meta.subject],
    secondary: [meta.message, meta.author],
  }
}

/** Word-boundary at `index` (code-unit offset into a lowercased JS string). */
function isWordBoundaryAt(field: string, index: number): boolean {
  if (index <= 0) return true
  let i = index - 1
  if (i > 0) {
    const c = field.charCodeAt(i)
    const prev = field.charCodeAt(i - 1)
    // Step back over a trailing surrogate pair.
    if (c >= 0xdc00 && c <= 0xdfff && prev >= 0xd800 && prev <= 0xdbff) {
      i -= 1
    }
  }
  const ch = field[i]!
  // Mirror Rust `!prev.is_alphanumeric() && prev != '_'`.
  const isAlphanumeric = /[\p{L}\p{N}]/u.test(ch)
  return !isAlphanumeric && ch !== "_"
}

function literalPrimaryTier(
  patternLowered: string,
  field: string
): number | null {
  const fieldLowered = field.toLowerCase()
  if (fieldLowered === patternLowered) return 0
  if (fieldLowered.startsWith(patternLowered)) return 1

  let searchFrom = 0
  let foundAny = false
  while (searchFrom <= fieldLowered.length) {
    const rel = fieldLowered.indexOf(patternLowered, searchFrom)
    if (rel < 0) break
    foundAny = true
    if (isWordBoundaryAt(fieldLowered, rel)) return 2
    // Advance by one Unicode scalar.
    const cp = fieldLowered.codePointAt(rel)
    const advance = cp !== undefined ? (cp > 0xffff ? 2 : 1) : 1
    searchFrom = rel + advance
  }
  return foundAny ? 3 : null
}

/**
 * Literal field rank: tiers 0–4 (lower is better), or null when no match.
 * Declared-order tie-breaking matches the Rust matcher.
 */
export function rankLiteralFields(
  query: string,
  primary: readonly string[],
  secondary: readonly string[]
): number | null {
  if (!query) return null
  const lowered = query.toLowerCase()
  if (!lowered) return null

  let bestTier: number | null = null
  let bestIndex = Number.POSITIVE_INFINITY
  const consider = (tier: number, index: number) => {
    if (
      bestTier == null ||
      tier < bestTier ||
      (tier === bestTier && index < bestIndex)
    ) {
      bestTier = tier
      bestIndex = index
    }
  }

  for (let i = 0; i < primary.length; i++) {
    const tier = literalPrimaryTier(lowered, primary[i]!)
    if (tier != null) consider(tier, i)
  }
  for (let i = 0; i < secondary.length; i++) {
    if (secondary[i]!.toLowerCase().includes(lowered)) {
      consider(4, i)
    }
  }
  return bestTier
}

function cloneCandidate(candidate: ReferenceCandidate): ReferenceCandidate {
  return {
    source: candidate.source,
    uri: candidate.uri,
    id: candidate.id,
    label: candidate.label,
    detail: candidate.detail,
    keywords: candidate.keywords,
    metadata: { ...candidate.metadata },
    sourceOrdinal: candidate.sourceOrdinal,
    // Ranks live only in regex snapshots.
    regexRank: null,
  }
}

function conversationLabelFromSummary(summary: DbConversationSummary): string {
  const folded = formatConversationTitle(summary.title).trim()
  return folded || `#${summary.id}`
}

function buildConversationDetail(
  branch: string | null,
  status: string
): string {
  return branch ?? status
}

export class ReferenceSearchCache {
  private readonly candidateCap: number
  private readonly candidateByteCap: number
  private readonly regexSnapshotCap: number

  private mutationClock = 0
  private candidateCount = 0
  private candidateBytes = 0
  private lruClock = 0
  private refreshTokenClock = 0

  private readonly buckets = new Map<string, Map<string, ItemEntry>>()
  private readonly regexSnapshots = new Map<
    string,
    Map<string, RegexSnapshot>
  >()
  private readonly pinCounts = new Map<string, number>()
  private readonly controllers = new Map<string, ControllerState>()
  private readonly activeRefreshes = new Map<number, ActiveRegexRefresh>()
  private readonly fileAliases = new Map<string, string>()
  private readonly tombstones = new Map<string, Set<string>>()
  private readonly eventWatermarks = new Map<string, number>()
  private readonly eventWatermarkOrder: string[] = []
  private readonly listeners = new Set<ConversationChangeListener>()

  constructor(options: ReferenceSearchCacheOptions = {}) {
    this.candidateCap = options.candidateCap ?? DEFAULT_CANDIDATE_CAP
    this.candidateByteCap =
      options.candidateByteCap ?? DEFAULT_CANDIDATE_BYTE_CAP
    this.regexSnapshotCap =
      options.regexSnapshotCap ?? DEFAULT_REGEX_SNAPSHOT_CAP
  }

  captureMutationRevision(): number {
    return this.mutationClock
  }

  mergeCandidate(
    bucket: ReferenceCacheBucketKey,
    candidate: ReferenceCandidate,
    options?: { conversationPageStartedAt?: number }
  ): CachedCandidate | null {
    const bucketKey = serializeBucketKey(bucket)
    if (bucket.source === "conversation") {
      if (this.isTombstoned(bucket.backend, candidate.uri)) {
        return null
      }
      if (options?.conversationPageStartedAt != null) {
        const watermark = this.eventWatermarks.get(
          this.watermarkKey(bucket.backend, candidate.uri)
        )
        if (
          watermark != null &&
          watermark > options.conversationPageStartedAt
        ) {
          return null
        }
      }
    }

    const cloned = cloneCandidate(candidate)
    const bytes = candidateRetainedUtf8Bytes(cloned)
    const rev = ++this.mutationClock

    let bucketMap = this.buckets.get(bucketKey)
    if (!bucketMap) {
      bucketMap = new Map()
      this.buckets.set(bucketKey, bucketMap)
    }

    const existing = bucketMap.get(cloned.uri)
    if (existing) {
      this.candidateBytes -= existing.retainedUtf8Bytes
      this.candidateCount -= 1
    }

    const entry: ItemEntry = {
      candidate: cloned,
      mutationRevision: rev,
      retainedUtf8Bytes: bytes,
      lruTick: ++this.lruClock,
    }
    bucketMap.set(cloned.uri, entry)
    this.candidateCount += 1
    this.candidateBytes += bytes
    this.pruneCandidates()

    // Entry may have been pruned if unpinned and over cap (shouldn't for
    // single insert unless bytes alone exceed and unpinned — still return it
    // only if present).
    const live = this.buckets.get(bucketKey)?.get(cloned.uri)
    if (!live) {
      // Pinned overflow always retains; if gone, return the logical merge
      // result for the caller (authoritative page still "accepted").
      return {
        candidate: cloned,
        mutationRevision: rev,
        stale: false,
      }
    }
    return {
      candidate: live.candidate,
      mutationRevision: live.mutationRevision,
      stale: false,
    }
  }

  literalPreview(
    bucket: ReferenceCacheBucketKey,
    query: string,
    limit: number
  ): LiteralCachePreview {
    const bucketKey = serializeBucketKey(bucket)
    const bucketMap = this.buckets.get(bucketKey)
    if (!bucketMap || limit <= 0) {
      return { items: [], truncated: false }
    }

    const matched: { entry: ItemEntry; rank: number }[] = []
    for (const entry of bucketMap.values()) {
      const fields = candidateSearchFields(entry.candidate)
      const rank = rankLiteralFields(query, fields.primary, fields.secondary)
      if (rank != null) {
        matched.push({ entry, rank })
      }
    }
    matched.sort((a, b) => {
      if (a.rank !== b.rank) return a.rank - b.rank
      if (a.entry.candidate.sourceOrdinal !== b.entry.candidate.sourceOrdinal) {
        return a.entry.candidate.sourceOrdinal - b.entry.candidate.sourceOrdinal
      }
      return a.entry.candidate.uri.localeCompare(b.entry.candidate.uri)
    })
    const truncated = matched.length > limit
    const items = matched.slice(0, limit).map(({ entry }) => ({
      candidate: entry.candidate,
      mutationRevision: entry.mutationRevision,
      stale: false,
    }))
    return { items, truncated }
  }

  getRegexSnapshot(
    bucket: ReferenceCacheBucketKey,
    expression: string
  ): LiteralCachePreview | null {
    const bucketKey = serializeBucketKey(bucket)
    const snap = this.regexSnapshots.get(bucketKey)?.get(expression)
    if (!snap) return null
    snap.lruTick = ++this.lruClock

    const bucketMap = this.buckets.get(bucketKey)
    const items: CachedCandidate[] = []
    for (const ref of snap.refs) {
      if (ref.stale) continue
      const entry = bucketMap?.get(ref.uri)
      if (!entry) continue
      items.push({
        candidate: {
          ...entry.candidate,
          regexRank: ref.rank,
        },
        mutationRevision: entry.mutationRevision,
        stale: false,
      })
    }
    return { items, truncated: snap.truncated }
  }

  beginRegexRefresh(
    controllerId: string,
    bucket: ReferenceCacheBucketKey,
    expression: string
  ): RegexRefreshHandle {
    const bucketKey = serializeBucketKey(bucket)
    // Supersede any prior active refresh for this controller+bucket.
    for (const [token, active] of this.activeRefreshes) {
      if (
        active.handle.controllerId === controllerId &&
        active.handle.bucketKey === bucketKey
      ) {
        this.activeRefreshes.delete(token)
      }
    }

    const revisionSnapshot = new Map<string, number>()
    const bucketMap = this.buckets.get(bucketKey)
    if (bucketMap) {
      for (const [uri, entry] of bucketMap) {
        revisionSnapshot.set(uri, entry.mutationRevision)
      }
    }

    const handle: RegexRefreshHandle = {
      token: ++this.refreshTokenClock,
      controllerId,
      bucketKey,
      expression,
    }
    this.activeRefreshes.set(handle.token, {
      handle,
      backend: bucket.backend,
      revisionSnapshot,
    })

    const ctl = this.ensureController(controllerId)
    ctl.activeRegex.set(bucketKey, expression)
    this.pruneRegexSnapshots()
    return handle
  }

  commitRegexRefresh(
    refresh: RegexRefreshHandle,
    items: readonly { uri: string; rank: ReferenceRegexRank }[],
    truncated: boolean
  ): void {
    const active = this.activeRefreshes.get(refresh.token)
    if (!active) return
    this.activeRefreshes.delete(refresh.token)

    const { bucketKey, expression, controllerId } = refresh
    const bucketMap = this.buckets.get(bucketKey)
    const refs: RegexRef[] = []
    for (const item of items) {
      const entry = bucketMap?.get(item.uri)
      if (!entry) continue
      if (
        entry.candidate.source === "conversation" &&
        this.isTombstoned(active.backend, item.uri)
      ) {
        continue
      }
      const snapRev = active.revisionSnapshot.get(item.uri)
      // Revision advanced while the source drained → keep rank but mark stale.
      const stale = snapRev == null || snapRev !== entry.mutationRevision
      refs.push({
        uri: item.uri,
        rank: { ...item.rank },
        stale,
        mutationRevision: snapRev ?? entry.mutationRevision,
      })
    }

    let byBucket = this.regexSnapshots.get(bucketKey)
    if (!byBucket) {
      byBucket = new Map()
      this.regexSnapshots.set(bucketKey, byBucket)
    }
    byBucket.set(expression, {
      refs,
      truncated,
      lruTick: ++this.lruClock,
    })

    const ctl = this.controllers.get(controllerId)
    if (ctl?.activeRegex.get(bucketKey) === expression) {
      ctl.activeRegex.delete(bucketKey)
      if (this.isControllerEmpty(ctl)) {
        this.controllers.delete(controllerId)
      }
    }
    this.pruneRegexSnapshots()
  }

  discardRegexRefresh(refresh: RegexRefreshHandle): void {
    const active = this.activeRefreshes.get(refresh.token)
    if (!active) return
    this.activeRefreshes.delete(refresh.token)
    const ctl = this.controllers.get(refresh.controllerId)
    if (ctl?.activeRegex.get(refresh.bucketKey) === refresh.expression) {
      ctl.activeRegex.delete(refresh.bucketKey)
      if (this.isControllerEmpty(ctl)) {
        this.controllers.delete(refresh.controllerId)
      }
    }
    this.pruneRegexSnapshots()
  }

  markConversationUpsert(
    backend: string,
    summary: DbConversationSummary
  ): void {
    const uri = sessionUri(summary.id)
    if (this.isTombstoned(backend, uri)) return

    const rev = ++this.mutationClock
    this.recordEventWatermark(backend, uri, rev)

    const bucketKey = serializeBucketKey({ backend, source: "conversation" })
    const entry = this.buckets.get(bucketKey)?.get(uri)
    let mutationRevision: number | null = null

    if (entry && entry.candidate.metadata.kind === "conversation") {
      const label = conversationLabelFromSummary(summary)
      const branch = summary.git_branch
      const status = summary.status
      const meta = entry.candidate.metadata
      entry.candidate = {
        ...entry.candidate,
        label,
        detail: buildConversationDetail(branch, status),
        keywords: `${label} ${summary.agent_type}`,
        metadata: {
          ...meta,
          agentType: summary.agent_type,
          status,
          branch,
          // Preserve joined project fields.
          projectName: meta.projectName,
          projectPath: meta.projectPath,
        },
        regexRank: null,
      }
      const newBytes = candidateRetainedUtf8Bytes(entry.candidate)
      this.candidateBytes -= entry.retainedUtf8Bytes
      entry.retainedUtf8Bytes = newBytes
      this.candidateBytes += newBytes
      entry.mutationRevision = rev
      entry.lruTick = ++this.lruClock
      mutationRevision = rev
      this.markRegexRefsStale(bucketKey, uri)
    }

    this.publish({
      kind: "upsert",
      backend,
      uri,
      summary,
      mutationRevision,
    })
  }

  markConversationStatus(
    backend: string,
    conversationId: number,
    status: string
  ): void {
    const uri = sessionUri(conversationId)
    if (this.isTombstoned(backend, uri)) return

    const rev = ++this.mutationClock
    this.recordEventWatermark(backend, uri, rev)

    const bucketKey = serializeBucketKey({ backend, source: "conversation" })
    const entry = this.buckets.get(bucketKey)?.get(uri)
    let mutationRevision: number | null = null

    if (entry && entry.candidate.metadata.kind === "conversation") {
      const meta = entry.candidate.metadata
      const branch = meta.branch
      entry.candidate = {
        ...entry.candidate,
        detail: branch == null ? status : entry.candidate.detail,
        metadata: {
          ...meta,
          status,
        },
        regexRank: null,
      }
      const newBytes = candidateRetainedUtf8Bytes(entry.candidate)
      this.candidateBytes -= entry.retainedUtf8Bytes
      entry.retainedUtf8Bytes = newBytes
      this.candidateBytes += newBytes
      entry.mutationRevision = rev
      entry.lruTick = ++this.lruClock
      mutationRevision = rev
      this.markRegexRefsStale(bucketKey, uri)
    }

    this.publish({
      kind: "status",
      backend,
      uri,
      status,
      mutationRevision,
    })
  }

  markConversationDelete(backend: string, conversationId: number): void {
    const uri = sessionUri(conversationId)
    const rev = ++this.mutationClock
    this.recordEventWatermark(backend, uri, rev)
    this.addTombstone(backend, uri)

    const bucketKey = serializeBucketKey({ backend, source: "conversation" })
    this.removeUriEverywhere(bucketKey, uri)
    this.publish({ kind: "delete", backend, uri })
  }

  markConversationNotFoundIfRevision(
    backend: string,
    uri: string,
    mutationRevision: number
  ): boolean {
    const bucketKey = serializeBucketKey({ backend, source: "conversation" })
    const entry = this.buckets.get(bucketKey)?.get(uri)
    if (!entry || entry.mutationRevision !== mutationRevision) {
      return false
    }
    const rev = ++this.mutationClock
    this.recordEventWatermark(backend, uri, rev)
    this.addTombstone(backend, uri)
    this.removeUriEverywhere(bucketKey, uri)
    this.publish({ kind: "delete", backend, uri })
    return true
  }

  subscribeConversationChanges(
    listener: ConversationChangeListener
  ): () => void {
    this.listeners.add(listener)
    let active = true
    return () => {
      if (!active) return
      active = false
      this.listeners.delete(listener)
    }
  }

  evictUri(bucket: ReferenceCacheBucketKey, uri: string): void {
    const bucketKey = serializeBucketKey(bucket)
    this.removeUriEverywhere(bucketKey, uri)
  }

  evictIfRevision(
    bucket: ReferenceCacheBucketKey,
    uri: string,
    mutationRevision: number
  ): boolean {
    const bucketKey = serializeBucketKey(bucket)
    const entry = this.buckets.get(bucketKey)?.get(uri)
    if (!entry || entry.mutationRevision !== mutationRevision) return false
    this.removeUriEverywhere(bucketKey, uri)
    return true
  }

  pinVisible(
    controllerId: string,
    bucket: ReferenceCacheBucketKey,
    uris: readonly string[]
  ): void {
    const bucketKey = serializeBucketKey(bucket)
    const ctl = this.ensureController(controllerId)
    const prev = ctl.visible.get(bucketKey)
    if (prev) {
      for (const uri of prev) {
        this.decPin(bucketKey, uri)
      }
    }
    const next = new Set(uris)
    if (next.size === 0) {
      ctl.visible.delete(bucketKey)
    } else {
      ctl.visible.set(bucketKey, next)
      for (const uri of next) {
        this.incPin(bucketKey, uri)
      }
    }
    if (this.isControllerEmpty(ctl)) {
      this.controllers.delete(controllerId)
    }
    this.pruneCandidates()
  }

  pinSelected(
    controllerId: string,
    bucket: ReferenceCacheBucketKey,
    uri: string | null
  ): void {
    const bucketKey = serializeBucketKey(bucket)
    const ctl = this.ensureController(controllerId)
    if (ctl.selected) {
      this.decPin(ctl.selected.bucketKey, ctl.selected.uri)
      ctl.selected = null
    }
    if (uri != null) {
      ctl.selected = { bucketKey, uri }
      this.incPin(bucketKey, uri)
    }
    if (this.isControllerEmpty(ctl)) {
      this.controllers.delete(controllerId)
    }
    this.pruneCandidates()
  }

  releaseController(controllerId: string): void {
    const ctl = this.controllers.get(controllerId)
    if (!ctl) {
      // Still drop any active refreshes for this controller.
      for (const [token, active] of this.activeRefreshes) {
        if (active.handle.controllerId === controllerId) {
          this.activeRefreshes.delete(token)
        }
      }
      this.pruneCandidates()
      this.pruneRegexSnapshots()
      return
    }
    for (const [bucketKey, uris] of ctl.visible) {
      for (const uri of uris) this.decPin(bucketKey, uri)
    }
    if (ctl.selected) {
      this.decPin(ctl.selected.bucketKey, ctl.selected.uri)
    }
    this.controllers.delete(controllerId)
    for (const [token, active] of this.activeRefreshes) {
      if (active.handle.controllerId === controllerId) {
        this.activeRefreshes.delete(token)
      }
    }
    this.pruneCandidates()
    this.pruneRegexSnapshots()
  }

  rememberFileRootAlias(
    backend: string,
    requestedRoot: string,
    canonicalRoot: string
  ): void {
    const key = this.aliasKey(backend, requestedRoot)
    // Repointing replaces the mapping only; old canonical bucket is retained.
    this.fileAliases.set(key, canonicalRoot)
  }

  resolveFileRootAlias(backend: string, requestedRoot: string): string | null {
    return this.fileAliases.get(this.aliasKey(backend, requestedRoot)) ?? null
  }

  reset(): void {
    this.buckets.clear()
    this.regexSnapshots.clear()
    this.pinCounts.clear()
    this.controllers.clear()
    this.activeRefreshes.clear()
    this.fileAliases.clear()
    this.tombstones.clear()
    this.eventWatermarks.clear()
    this.eventWatermarkOrder.length = 0
    this.candidateCount = 0
    this.candidateBytes = 0
    // mutationClock intentionally preserved across reset.
  }

  /** Test-only: whether a URI is present in the bucket. */
  has(bucket: ReferenceCacheBucketKey, uri: string): boolean {
    return this.buckets.get(serializeBucketKey(bucket))?.has(uri) ?? false
  }

  /** Test-only: content-free counters. */
  debugStats(): { candidateCount: number; candidateBytes: number } {
    return {
      candidateCount: this.candidateCount,
      candidateBytes: this.candidateBytes,
    }
  }

  private ensureController(controllerId: string): ControllerState {
    let ctl = this.controllers.get(controllerId)
    if (!ctl) {
      ctl = {
        visible: new Map(),
        selected: null,
        activeRegex: new Map(),
      }
      this.controllers.set(controllerId, ctl)
    }
    return ctl
  }

  private isControllerEmpty(ctl: ControllerState): boolean {
    return (
      ctl.visible.size === 0 &&
      ctl.selected == null &&
      ctl.activeRegex.size === 0
    )
  }

  private pinKey(bucketKey: string, uri: string): string {
    return `${bucketKey}\n${uri}`
  }

  private incPin(bucketKey: string, uri: string): void {
    const key = this.pinKey(bucketKey, uri)
    this.pinCounts.set(key, (this.pinCounts.get(key) ?? 0) + 1)
  }

  private decPin(bucketKey: string, uri: string): void {
    const key = this.pinKey(bucketKey, uri)
    const next = (this.pinCounts.get(key) ?? 0) - 1
    if (next <= 0) this.pinCounts.delete(key)
    else this.pinCounts.set(key, next)
  }

  private isPinned(bucketKey: string, uri: string): boolean {
    return (this.pinCounts.get(this.pinKey(bucketKey, uri)) ?? 0) > 0
  }

  private pruneCandidates(): void {
    while (
      this.candidateCount > this.candidateCap ||
      this.candidateBytes > this.candidateByteCap
    ) {
      let victim: { bucketKey: string; uri: string; tick: number } | null = null
      for (const [bucketKey, map] of this.buckets) {
        for (const [uri, entry] of map) {
          if (this.isPinned(bucketKey, uri)) continue
          if (!victim || entry.lruTick < victim.tick) {
            victim = { bucketKey, uri, tick: entry.lruTick }
          }
        }
      }
      if (!victim) {
        // All remaining are pinned — temporary overflow.
        break
      }
      this.removeUriEverywhere(victim.bucketKey, victim.uri)
    }
    this.removeEmptyBuckets()
  }

  private isExpressionPinned(bucketKey: string, expression: string): boolean {
    for (const ctl of this.controllers.values()) {
      if (ctl.activeRegex.get(bucketKey) === expression) return true
    }
    for (const active of this.activeRefreshes.values()) {
      if (
        active.handle.bucketKey === bucketKey &&
        active.handle.expression === expression
      ) {
        return true
      }
    }
    return false
  }

  private pruneRegexSnapshots(): void {
    const countSnapshots = () => {
      let n = 0
      for (const m of this.regexSnapshots.values()) n += m.size
      return n
    }
    while (countSnapshots() > this.regexSnapshotCap) {
      let victim: {
        bucketKey: string
        expression: string
        tick: number
      } | null = null
      for (const [bucketKey, map] of this.regexSnapshots) {
        for (const [expression, snap] of map) {
          if (this.isExpressionPinned(bucketKey, expression)) continue
          if (!victim || snap.lruTick < victim.tick) {
            victim = { bucketKey, expression, tick: snap.lruTick }
          }
        }
      }
      if (!victim) break
      const map = this.regexSnapshots.get(victim.bucketKey)
      map?.delete(victim.expression)
      if (map && map.size === 0) this.regexSnapshots.delete(victim.bucketKey)
    }
  }

  private removeUriEverywhere(bucketKey: string, uri: string): void {
    const bucketMap = this.buckets.get(bucketKey)
    const entry = bucketMap?.get(uri)
    if (entry) {
      bucketMap!.delete(uri)
      this.candidateCount -= 1
      this.candidateBytes -= entry.retainedUtf8Bytes
      if (bucketMap!.size === 0) this.buckets.delete(bucketKey)
    }

    const snaps = this.regexSnapshots.get(bucketKey)
    if (snaps) {
      for (const [expr, snap] of snaps) {
        const next = snap.refs.filter((r) => r.uri !== uri)
        if (next.length !== snap.refs.length) {
          snap.refs = next
        }
        if (snap.refs.length === 0) {
          // Keep empty snapshot shells? Drop them.
          snaps.delete(expr)
        }
      }
      if (snaps.size === 0) this.regexSnapshots.delete(bucketKey)
    }

    // Drop dangling pin memberships for this URI.
    for (const [controllerId, ctl] of this.controllers) {
      const visible = ctl.visible.get(bucketKey)
      if (visible?.has(uri)) {
        visible.delete(uri)
        this.decPin(bucketKey, uri)
        if (visible.size === 0) ctl.visible.delete(bucketKey)
      }
      if (ctl.selected?.bucketKey === bucketKey && ctl.selected.uri === uri) {
        this.decPin(bucketKey, uri)
        ctl.selected = null
      }
      if (this.isControllerEmpty(ctl)) {
        this.controllers.delete(controllerId)
      }
    }
    this.removeEmptyBuckets()
  }

  private removeEmptyBuckets(): void {
    for (const [bucketKey, map] of this.buckets) {
      if (map.size === 0) this.buckets.delete(bucketKey)
    }
    // Drop file aliases whose canonical root bucket no longer exists.
    for (const [aliasKey, canonical] of this.fileAliases) {
      const sep = aliasKey.indexOf("\n")
      if (sep < 0) continue
      const backend = aliasKey.slice(0, sep)
      const fileKey = serializeBucketKey({
        backend,
        source: "file",
        canonicalRoot: canonical,
      })
      if (!this.buckets.has(fileKey)) {
        this.fileAliases.delete(aliasKey)
      }
    }
  }

  private markRegexRefsStale(bucketKey: string, uri: string): void {
    const snaps = this.regexSnapshots.get(bucketKey)
    if (!snaps) return
    for (const snap of snaps.values()) {
      for (const ref of snap.refs) {
        if (ref.uri === uri) ref.stale = true
      }
    }
  }

  private aliasKey(backend: string, requestedRoot: string): string {
    return `${backend}\n${requestedRoot}`
  }

  private watermarkKey(backend: string, uri: string): string {
    return `${backend}\n${uri}`
  }

  private recordEventWatermark(
    backend: string,
    uri: string,
    revision: number
  ): void {
    const key = this.watermarkKey(backend, uri)
    if (this.eventWatermarks.has(key)) {
      const idx = this.eventWatermarkOrder.indexOf(key)
      if (idx >= 0) this.eventWatermarkOrder.splice(idx, 1)
    }
    this.eventWatermarks.set(key, revision)
    this.eventWatermarkOrder.push(key)
    while (this.eventWatermarkOrder.length > EVENT_WATERMARK_CAP) {
      const oldest = this.eventWatermarkOrder.shift()
      if (oldest) this.eventWatermarks.delete(oldest)
    }
  }

  private isTombstoned(backend: string, uri: string): boolean {
    return this.tombstones.get(backend)?.has(uri) ?? false
  }

  private addTombstone(backend: string, uri: string): void {
    let set = this.tombstones.get(backend)
    if (!set) {
      set = new Set()
      this.tombstones.set(backend, set)
    }
    // Refresh FIFO recency on duplicate.
    if (set.has(uri)) set.delete(uri)
    set.add(uri)
    while (set.size > CONVERSATION_TOMBSTONE_CAP) {
      const oldest = set.values().next().value
      if (oldest === undefined) break
      set.delete(oldest)
    }
  }

  private publish(change: ReferenceConversationCacheChange): void {
    for (const listener of this.listeners) {
      try {
        listener(change)
      } catch {
        // Listener failures must not prevent later listeners.
      }
    }
  }
}

export const referenceSearchCache = new ReferenceSearchCache()

// Register with the backend-scoped reset registry from the cache module itself
// so the method receiver is preserved.
registerBackendScopedStoreReset(() => referenceSearchCache.reset())
