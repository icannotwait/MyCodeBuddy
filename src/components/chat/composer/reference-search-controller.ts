import {
  cancelReferenceSearch,
  matchReferenceRegex,
  nextReferenceSearchPage,
  startReferenceSearch,
  validateReferenceCandidate,
} from "@/lib/api"
import { extractAppCommandError } from "@/lib/app-error"
import { formatConversationTitle } from "@/lib/conversation-title"
import {
  candidateSearchFields,
  rankLiteralFields,
  serializeBucketKey,
  type ReferenceCacheBucketKey,
  type ReferenceConversationCacheChange,
  type ReferenceSearchCache,
  type RegexRefreshHandle,
} from "@/lib/reference-search-cache"
import type {
  AcpAgentInfo,
  DelegationProfileCatalog,
  GitHeadInfo,
  ReferenceCandidate,
  ReferenceCandidateValidation,
  ReferenceDescriptor,
  ReferenceRegexMatch,
  ReferenceSearchPage,
  ReferenceSearchSource,
  StartReferenceSearchRequest,
  ValidateReferenceCandidateRequest,
} from "@/lib/types"
import { randomUUID } from "@/lib/utils"

import type { ReferenceAttrs } from "./types"
import {
  candidateToSuggestion,
  catalogEntryToSuggestion,
  catalogSearchFields,
  type CatalogSearchEntry,
} from "./suggestion/adapters"
import type { SuggestionItem } from "./suggestion/types"
import type { ReferenceGroupLabels } from "./use-reference-search"
import { DEFAULT_GROUP_LABELS } from "./use-reference-search"

const REGEX_BATCH_SIZE = 1024
const REGEX_CONCURRENCY = 4
const CONFIRM_TIMEOUT_MS = 1000

export type ReferenceGroupKind = "agent" | "file" | "session" | "commit"

export interface ReferenceGroupSnapshot {
  kind: ReferenceGroupKind
  label: string
  items: SuggestionItem[]
  loading: boolean
  truncated: boolean
  error: "profile" | "source" | null
}

export interface ReferenceSearchSnapshot {
  query: string
  generation: number
  patternError: boolean
  groups: Record<ReferenceGroupKind, ReferenceGroupSnapshot>
}

export interface ReferenceSearchControllerInputs {
  agents: AcpAgentInfo[]
  profileCatalog: DelegationProfileCatalog | null
  profileCatalogError: boolean
  referenceLimit: number
  gitHead: GitHeadInfo | null
  labels: ReferenceGroupLabels
}

export interface ReferenceSearchControllerDeps {
  backendKey: string
  folderId: number | null
  defaultPath: string | null
  cache: ReferenceSearchCache
  fetchGitHead: () => Promise<GitHeadInfo>
  applyGitHead: (head: GitHeadInfo) => void
  generateId?: () => string
  startReferenceSearch?: typeof startReferenceSearch
  nextReferenceSearchPage?: typeof nextReferenceSearchPage
  cancelReferenceSearch?: typeof cancelReferenceSearch
  validateReferenceCandidate?: typeof validateReferenceCandidate
  matchReferenceRegex?: typeof matchReferenceRegex
}

type ResourceSource = ReferenceSearchSource

interface LiveSourceIdentity {
  sourceSequence: number
  requestId: string
  pageInFlight: boolean
  abort: AbortController
  conversationPageStartedAt?: number
  /** Canonical root established by the first non-empty file page of this drain. */
  fileCanonicalRoot?: string
}

interface MembershipEntry {
  item: SuggestionItem
  mutationRevision: number | null
  bucketKey: string | null
  source: ResourceSource | "catalog"
  literalRank: number | null
}

interface ValidationState {
  validationRequestId: string
  bucket: ReferenceCacheBucketKey
  generation: number
  selectedUri: string
  mutationRevision: number
  abort: AbortController
  promise: Promise<ReferenceAttrs | null>
}

interface GitRefreshGate {
  generation: number
  oldCommitIdentity: string
  promise: Promise<void>
}

interface RegexDrain {
  handle: RegexRefreshHandle
  items: { uri: string; rank: NonNullable<SuggestionItem["regexRank"]> }[]
}

function isRegexQuery(query: string): boolean {
  return query.startsWith("re:")
}

function errorCode(error: unknown): string | null {
  if (
    typeof DOMException !== "undefined" &&
    error instanceof DOMException &&
    error.name === "AbortError"
  ) {
    return "cancelled"
  }
  if (error instanceof Error && error.name === "AbortError") {
    return "cancelled"
  }
  return extractAppCommandError(error)?.code ?? null
}

function commitIdentityKey(head: GitHeadInfo | null): string {
  if (!head) return "null"
  return [
    head.is_repo ? "1" : "0",
    head.canonical_repo ?? "",
    head.head_sha ?? "",
    head.reference_source_epoch ?? "",
  ].join("\0")
}

function emptyGroup(
  kind: ReferenceGroupKind,
  label: string
): ReferenceGroupSnapshot {
  return {
    kind,
    label,
    items: [],
    loading: false,
    truncated: false,
    error: null,
  }
}

function compareLiteral(
  a: { rank: number | null; ordinal: number; uri: string },
  b: { rank: number | null; ordinal: number; uri: string }
): number {
  const ar = a.rank ?? Number.POSITIVE_INFINITY
  const br = b.rank ?? Number.POSITIVE_INFINITY
  if (ar !== br) return ar - br
  if (a.ordinal !== b.ordinal) return a.ordinal - b.ordinal
  return a.uri.localeCompare(b.uri)
}

function compareRegex(
  a: {
    rank: SuggestionItem["regexRank"]
    ordinal: number
    uri: string
  },
  b: {
    rank: SuggestionItem["regexRank"]
    ordinal: number
    uri: string
  }
): number {
  const ar = a.rank
  const br = b.rank
  if (!ar && !br) {
    if (a.ordinal !== b.ordinal) return a.ordinal - b.ordinal
    return a.uri.localeCompare(b.uri)
  }
  if (!ar) return 1
  if (!br) return -1
  if (ar.fieldTier !== br.fieldTier) return ar.fieldTier - br.fieldTier
  if (ar.start !== br.start) return ar.start - br.start
  if (ar.length !== br.length) return ar.length - br.length
  if (a.ordinal !== b.ordinal) return a.ordinal - b.ordinal
  return a.uri.localeCompare(b.uri)
}

function catalogUri(entry: CatalogSearchEntry): string {
  if (entry.kind === "agent") {
    return `codeg://agent/${entry.agent.agent_type}`
  }
  return `codeg://delegation-profile/${entry.profile.agent_type}/${entry.profile.id}`
}

export class ReferenceSearchController {
  readonly searchSessionId: string
  private readonly backendKey: string
  private readonly folderId: number | null
  private readonly defaultPath: string | null
  private readonly cache: ReferenceSearchCache
  private readonly fetchGitHead: () => Promise<GitHeadInfo>
  private readonly applyGitHeadExternal: (head: GitHeadInfo) => void
  private readonly generateId: () => string
  private readonly api: {
    start: typeof startReferenceSearch
    next: typeof nextReferenceSearchPage
    cancel: typeof cancelReferenceSearch
    validate: typeof validateReferenceCandidate
    matchRegex: typeof matchReferenceRegex
  }

  private active = false
  private generation = 0
  private catalogRunGeneration = 0
  private query = ""
  private inputs: ReferenceSearchControllerInputs = {
    agents: [],
    profileCatalog: null,
    profileCatalogError: false,
    referenceLimit: 50,
    gitHead: null,
    labels: DEFAULT_GROUP_LABELS,
  }

  private sequences: Record<ResourceSource, number> = {
    file: 0,
    conversation: 0,
    commit: 0,
  }
  private live: Partial<Record<ResourceSource, LiveSourceIdentity>> = {}
  private sourceLoading: Record<ResourceSource, boolean> = {
    file: false,
    conversation: false,
    commit: false,
  }
  private sourceError: Record<ResourceSource, boolean> = {
    file: false,
    conversation: false,
    commit: false,
  }
  private sourceTruncated: Record<ResourceSource, boolean> = {
    file: false,
    conversation: false,
    commit: false,
  }
  private patternError = false
  private agentLoading = false
  private agentError: "profile" | "source" | null = null
  private agentTruncated = false

  private membership = new Map<
    ReferenceGroupKind,
    Map<string, MembershipEntry>
  >([
    ["agent", new Map()],
    ["file", new Map()],
    ["session", new Map()],
    ["commit", new Map()],
  ])

  /** Last pinned bucket object per serialized key (needed to unpin abandoned aliases). */
  private pinnedBuckets = new Map<string, ReferenceCacheBucketKey>()
  private selectedUri: string | null = null
  private validation: ValidationState | null = null
  private gitRefresh: GitRefreshGate | null = null
  private conversationUnsub: (() => void) | null = null
  private listeners = new Set<() => void>()
  private snapshot: ReferenceSearchSnapshot

  private regexDrains: Partial<Record<ResourceSource, RegexDrain>> = {}
  private catalogEntries: { entry: CatalogSearchEntry; ordinal: number }[] = []

  private fileBucket: ReferenceCacheBucketKey | null = null
  private conversationBucket: ReferenceCacheBucketKey
  private commitBucket: ReferenceCacheBucketKey | null = null

  constructor(deps: ReferenceSearchControllerDeps) {
    this.backendKey = deps.backendKey
    this.folderId = deps.folderId
    this.defaultPath = deps.defaultPath
    this.cache = deps.cache
    this.fetchGitHead = deps.fetchGitHead
    this.applyGitHeadExternal = deps.applyGitHead
    this.generateId = deps.generateId ?? randomUUID
    this.searchSessionId = this.generateId()
    this.api = {
      start: deps.startReferenceSearch ?? startReferenceSearch,
      next: deps.nextReferenceSearchPage ?? nextReferenceSearchPage,
      cancel: deps.cancelReferenceSearch ?? cancelReferenceSearch,
      validate: deps.validateReferenceCandidate ?? validateReferenceCandidate,
      matchRegex: deps.matchReferenceRegex ?? matchReferenceRegex,
    }
    this.conversationBucket = {
      backend: this.backendKey,
      source: "conversation",
    }
    this.snapshot = this.buildEmptySnapshot()
  }

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener)
    return () => {
      this.listeners.delete(listener)
    }
  }

  getSnapshot(): ReferenceSearchSnapshot {
    return this.snapshot
  }

  setQuery(query: string): void {
    const wasActive = this.active
    this.active = true
    this.generation += 1
    this.query = query
    this.patternError = false
    this.clearGitRefreshGate()
    this.invalidateValidation()
    this.guardedCancelAllSources()
    this.discardAllRegexDrains()
    this.catalogRunGeneration += 1

    if (!wasActive) {
      this.acquireConversationSubscription()
    }

    this.resetResourceMembership()
    this.sourceLoading = { file: false, conversation: false, commit: false }
    this.sourceError = { file: false, conversation: false, commit: false }
    this.sourceTruncated = { file: false, conversation: false, commit: false }
    this.agentLoading = false
    this.agentError = this.inputs.profileCatalogError ? "profile" : null
    this.agentTruncated = false

    this.rebuildCatalogEntries()
    this.projectCatalogForQuery()
    this.projectResourceCachesForQuery()
    this.publish()

    if (query !== "") {
      this.startResourcesForCurrentQuery()
      if (isRegexQuery(query)) {
        void this.runCatalogRegex(this.generation, this.catalogRunGeneration)
      }
    }
  }

  updateInputs(inputs: ReferenceSearchControllerInputs): void {
    const prev = this.inputs
    const agentsChanged = !sameAgents(prev.agents, inputs.agents)
    const catalogChanged = !sameCatalog(
      prev.profileCatalog,
      inputs.profileCatalog
    )
    const profileErrorChanged =
      prev.profileCatalogError !== inputs.profileCatalogError
    const limitChanged = prev.referenceLimit !== inputs.referenceLimit
    const labelsChanged = !sameLabels(prev.labels, inputs.labels)
    const gitChanged = !sameGitHead(prev.gitHead, inputs.gitHead)

    this.inputs = {
      agents: inputs.agents,
      profileCatalog: inputs.profileCatalog,
      profileCatalogError: inputs.profileCatalogError,
      referenceLimit: inputs.referenceLimit,
      gitHead: inputs.gitHead,
      labels: inputs.labels,
    }

    if (profileErrorChanged || agentsChanged || catalogChanged) {
      this.agentError = inputs.profileCatalogError ? "profile" : this.agentError
      if (this.agentError === "profile" && !inputs.profileCatalogError) {
        this.agentError = null
      }
      this.rebuildCatalogEntries()
      if (this.active) {
        if (isRegexQuery(this.query) && this.query !== "") {
          if (agentsChanged || catalogChanged) {
            this.catalogRunGeneration += 1
            this.projectCatalogForQuery()
            this.agentLoading = true
            if (this.agentError === "source") this.agentError = null
            void this.runCatalogRegex(
              this.generation,
              this.catalogRunGeneration
            )
          } else {
            this.projectCatalogForQuery()
          }
        } else {
          this.projectCatalogForQuery()
        }
      }
    }

    if (labelsChanged && !agentsChanged && !catalogChanged && this.active) {
      // Labels only — republish without restarting work.
    }

    if (limitChanged && this.active && this.query !== "") {
      this.generation += 1
      this.guardedCancelAllSources()
      this.discardAllRegexDrains()
      this.invalidateValidation()
      this.clearGitRefreshGate()
      this.resetResourceMembership()
      this.sourceError = { file: false, conversation: false, commit: false }
      this.sourceTruncated = { file: false, conversation: false, commit: false }
      this.projectResourceCachesForQuery()
      this.startResourcesForCurrentQuery()
      if (isRegexQuery(this.query)) {
        this.catalogRunGeneration += 1
        this.projectCatalogForQuery()
        void this.runCatalogRegex(this.generation, this.catalogRunGeneration)
      } else {
        this.projectCatalogForQuery()
      }
    }

    if (gitChanged) {
      this.handleGitHeadChange(prev.gitHead, inputs.gitHead)
    }

    if (
      agentsChanged ||
      catalogChanged ||
      profileErrorChanged ||
      labelsChanged ||
      limitChanged ||
      gitChanged
    ) {
      if (this.active || labelsChanged || profileErrorChanged) {
        this.publish()
      }
    }
  }

  setSelectedUri(uri: string | null): void {
    this.selectedUri = uri
    if (uri == null) {
      this.cache.pinSelected(
        this.searchSessionId,
        this.conversationBucket,
        null
      )
      this.invalidateValidation()
      this.clearValidatingMarks()
      this.publish()
      return
    }

    const located = this.locateMembership(uri)
    if (!located) {
      this.cache.pinSelected(
        this.searchSessionId,
        this.conversationBucket,
        null
      )
      this.invalidateValidation()
      this.publish()
      return
    }

    const bucket = this.bucketForGroup(located.group)
    if (bucket) {
      this.cache.pinSelected(this.searchSessionId, bucket, uri)
    }

    const entry = located.entry
    if (
      entry.source === "catalog" ||
      entry.item.freshness === "fresh" ||
      entry.item.selectable === false
    ) {
      this.invalidateValidation()
      this.clearValidatingMarks()
      this.publish()
      return
    }

    // Cache-only / non-fresh resource rows start validation.
    void this.beginValidation(uri, located.group, entry)
    this.publish()
  }

  async confirmCandidate(uri: string): Promise<ReferenceAttrs | null> {
    const located = this.locateMembership(uri)
    if (!located) return null
    const { entry, group } = located
    if (!entry.item.selectable) return null

    if (entry.source === "catalog" || entry.item.freshness === "fresh") {
      return { ...entry.item.reference }
    }

    if (
      this.validation &&
      this.validation.selectedUri === uri &&
      this.validation.generation === this.generation
    ) {
      return await this.withTimeout(this.validation.promise, CONFIRM_TIMEOUT_MS)
    }

    return await this.beginValidation(uri, group, entry)
  }

  close(): void {
    if (!this.active) return
    this.active = false
    this.generation += 1
    this.clearGitRefreshGate()
    this.invalidateValidation()
    this.guardedCancelAllSources()
    this.discardAllRegexDrains()
    this.releaseConversationSubscription()
    this.cache.releaseController(this.searchSessionId)
    this.pinnedBuckets.clear()
    this.selectedUri = null
    this.resetAllMembership()
    this.sourceLoading = { file: false, conversation: false, commit: false }
    this.sourceError = { file: false, conversation: false, commit: false }
    this.sourceTruncated = { file: false, conversation: false, commit: false }
    this.agentLoading = false
    this.patternError = false
    this.agentError = this.inputs.profileCatalogError ? "profile" : null
    this.publish()
  }

  // ── Catalog ───────────────────────────────────────────────────────────

  private rebuildCatalogEntries(): void {
    const agents = this.inputs.agents
    const enabledAgents = agents.filter((a) => a.enabled)
    const agentByType = new Map(agents.map((a) => [a.agent_type, a]))
    const catalog = this.inputs.profileCatalog
    const profiles = catalog?.profiles ?? []
    const delegationEnabled = catalog?.delegation_enabled ?? false

    const entries: { entry: CatalogSearchEntry; ordinal: number }[] = []
    let ordinal = 1
    for (const agent of enabledAgents) {
      entries.push({ entry: { kind: "agent", agent }, ordinal: ordinal++ })
      if (!delegationEnabled) continue
      for (const profile of profiles) {
        if (profile.agent_type !== agent.agent_type) continue
        if (!profile.enabled) continue
        const backing = agentByType.get(profile.agent_type)
        if (!backing?.enabled) continue
        entries.push({
          entry: { kind: "profile", profile, backingAgent: backing },
          ordinal: ordinal++,
        })
      }
    }
    this.catalogEntries = entries
  }

  private projectCatalogForQuery(): void {
    const map = this.membership.get("agent")!
    if (this.query !== "" && isRegexQuery(this.query)) {
      // Keep prior rows as continuity until the matcher run commits atomically.
      for (const entry of map.values()) {
        entry.item = {
          ...entry.item,
          selectable: false,
          freshness: "cache",
        }
      }
      return
    }
    map.clear()
    const q = this.query
    for (const { entry, ordinal } of this.catalogEntries) {
      const fields = catalogSearchFields(entry)
      let rank: number | null = 0
      if (q !== "") {
        rank = rankLiteralFields(q, fields.primary, fields.secondary)
        if (rank == null) continue
      }
      const item = catalogEntryToSuggestion(entry, ordinal, "fresh", true)
      map.set(item.reference.uri, {
        item,
        mutationRevision: null,
        bucketKey: null,
        source: "catalog",
        literalRank: rank,
      })
    }
    this.agentLoading = false
    if (this.agentError === "source") this.agentError = null
  }

  private async runCatalogRegex(
    generation: number,
    runGeneration: number
  ): Promise<void> {
    this.agentLoading = true
    this.publish()

    const descriptors: ReferenceDescriptor[] = this.catalogEntries.map(
      ({ entry, ordinal }) => {
        const fields = catalogSearchFields(entry)
        return {
          id: catalogUri(entry),
          sourceOrdinal: ordinal,
          primary: fields.primary,
          secondary: fields.secondary,
        }
      }
    )

    const batches: ReferenceDescriptor[][] = []
    for (let i = 0; i < descriptors.length; i += REGEX_BATCH_SIZE) {
      batches.push(descriptors.slice(i, i + REGEX_BATCH_SIZE))
    }

    const working = new Map<string, ReferenceRegexMatch>()
    const byId = new Map(descriptors.map((d) => [d.id, d]))

    try {
      await runWithConcurrency(batches, REGEX_CONCURRENCY, async (batch) => {
        if (
          !this.active ||
          this.generation !== generation ||
          this.catalogRunGeneration !== runGeneration
        ) {
          return
        }
        const matches = await this.api.matchRegex({
          query: this.query,
          descriptors: batch,
        })
        if (
          !this.active ||
          this.generation !== generation ||
          this.catalogRunGeneration !== runGeneration
        ) {
          return
        }
        for (const match of matches) {
          const expected = byId.get(match.id)
          if (!expected || expected.sourceOrdinal !== match.sourceOrdinal) {
            throw Object.assign(new Error("catalog regex protocol error"), {
              code: "invalid_request",
              message: "catalog regex protocol error",
            })
          }
          if (working.has(match.id)) {
            throw Object.assign(new Error("duplicate catalog regex id"), {
              code: "invalid_request",
              message: "duplicate catalog regex id",
            })
          }
          working.set(match.id, match)
        }
      })
    } catch (error) {
      if (
        !this.active ||
        this.generation !== generation ||
        this.catalogRunGeneration !== runGeneration
      ) {
        return
      }
      const code = errorCode(error)
      if (code === "cancelled") return
      this.agentLoading = false
      if (code === "invalid_pattern") {
        this.patternError = true
        this.markAgentContinuityNonSelectable()
      } else {
        this.agentError = "source"
        this.markAgentContinuityNonSelectable()
      }
      this.publish()
      return
    }

    if (
      !this.active ||
      this.generation !== generation ||
      this.catalogRunGeneration !== runGeneration
    ) {
      return
    }

    const map = this.membership.get("agent")!
    map.clear()
    for (const { entry, ordinal } of this.catalogEntries) {
      const uri = catalogUri(entry)
      const match = working.get(uri)
      if (!match) continue
      const item = catalogEntryToSuggestion(entry, ordinal, "fresh", true)
      item.regexRank = match.rank
      map.set(uri, {
        item,
        mutationRevision: null,
        bucketKey: null,
        source: "catalog",
        literalRank: null,
      })
    }
    this.agentLoading = false
    if (this.agentError === "source") this.agentError = null
    this.publish()
  }

  private markAgentContinuityNonSelectable(): void {
    const map = this.membership.get("agent")!
    for (const entry of map.values()) {
      entry.item = {
        ...entry.item,
        selectable: false,
        freshness: "cache",
      }
    }
  }

  // ── Resource caches / membership ──────────────────────────────────────

  private projectResourceCachesForQuery(): void {
    if (this.query === "") {
      this.membership.get("file")!.clear()
      this.membership.get("session")!.clear()
      this.membership.get("commit")!.clear()
      return
    }

    const limit = this.inputs.referenceLimit
    const regex = isRegexQuery(this.query)

    // File
    this.membership.get("file")!.clear()
    this.fileBucket = null
    if (this.defaultPath) {
      const canonical = this.cache.resolveFileRootAlias(
        this.backendKey,
        this.defaultPath
      )
      if (canonical) {
        this.fileBucket = {
          backend: this.backendKey,
          source: "file",
          canonicalRoot: canonical,
        }
        this.applyCachePreview("file", this.fileBucket, regex, limit)
      }
    }

    // Conversation
    this.membership.get("session")!.clear()
    this.applyCachePreview("session", this.conversationBucket, regex, limit)

    // Commit
    this.membership.get("commit")!.clear()
    this.commitBucket = null
    const head = this.inputs.gitHead
    if (head?.is_repo && head.canonical_repo && head.reference_source_epoch) {
      this.commitBucket = {
        backend: this.backendKey,
        source: "commit",
        canonicalRepo: head.canonical_repo,
        sourceEpoch: head.reference_source_epoch,
      }
      this.applyCachePreview("commit", this.commitBucket, regex, limit)
    }
  }

  private applyCachePreview(
    group: "file" | "session" | "commit",
    bucket: ReferenceCacheBucketKey,
    regex: boolean,
    limit: number
  ): void {
    const map = this.membership.get(group)!
    if (regex) {
      const snap = this.cache.getRegexSnapshot(bucket, this.query)
      if (!snap) return
      for (const cached of snap.items) {
        const item = candidateToSuggestion(cached.candidate, "cache", true)
        map.set(item.reference.uri, {
          item,
          mutationRevision: cached.mutationRevision,
          bucketKey: serializeBucketKey(bucket),
          source: bucket.source,
          literalRank: null,
        })
      }
      if (snap.truncated) {
        this.sourceTruncated[bucket.source] = true
      }
      return
    }

    const preview = this.cache.literalPreview(bucket, this.query, limit)
    for (const cached of preview.items) {
      const fields = candidateSearchFields(cached.candidate)
      const rank = rankLiteralFields(
        this.query,
        fields.primary,
        fields.secondary
      )
      const item = candidateToSuggestion(cached.candidate, "cache", true)
      map.set(item.reference.uri, {
        item,
        mutationRevision: cached.mutationRevision,
        bucketKey: serializeBucketKey(bucket),
        source: bucket.source,
        literalRank: rank,
      })
    }
    if (preview.truncated) {
      this.sourceTruncated[bucket.source] = true
    }
  }

  private startResourcesForCurrentQuery(): void {
    if (this.defaultPath) {
      void this.drainSource("file")
    }
    void this.drainSource("conversation")
    if (
      this.folderId != null &&
      this.defaultPath &&
      (this.inputs.gitHead === null || this.inputs.gitHead.is_repo)
    ) {
      void this.drainSource("commit")
    }
  }

  private async drainSource(source: ResourceSource): Promise<void> {
    const generation = this.generation
    const sourceSequence = this.nextSequence(source)
    const requestId = this.generateId()
    const abort = new AbortController()
    const live: LiveSourceIdentity = {
      sourceSequence,
      requestId,
      pageInFlight: true,
      abort,
    }
    this.live[source] = live
    this.sourceLoading[source] = true
    this.sourceError[source] = false
    this.publish()

    if (isRegexQuery(this.query)) {
      const bucket = this.bucketForSource(source)
      if (bucket) {
        const handle = this.cache.beginRegexRefresh(
          this.searchSessionId,
          bucket,
          this.query
        )
        this.regexDrains[source] = { handle, items: [] }
      }
    }

    try {
      let pageIndex = 0
      if (source === "conversation") {
        live.conversationPageStartedAt = this.cache.captureMutationRevision()
      }
      const startReq: StartReferenceSearchRequest = {
        searchSessionId: this.searchSessionId,
        sourceSequence,
        requestId,
        source,
        query: this.query,
        workspacePath: source === "conversation" ? undefined : this.defaultPath,
      }
      let page = await this.api.start(startReq, abort.signal)
      while (true) {
        if (!this.isLive(source, generation, sourceSequence, requestId)) {
          return
        }
        const ok = this.ingestPage(source, generation, live, pageIndex, page)
        if (!ok) return

        const collected = this.collectedCount(source)
        if (page.done || collected >= this.inputs.referenceLimit) {
          break
        }
        pageIndex += 1
        if (source === "conversation") {
          live.conversationPageStartedAt = this.cache.captureMutationRevision()
        }
        page = await this.api.next(
          {
            searchSessionId: this.searchSessionId,
            sourceSequence,
            requestId,
            source,
            pageIndex,
          },
          abort.signal
        )
      }

      if (!this.isLive(source, generation, sourceSequence, requestId)) return
      this.finishRegexDrain(source)
      this.sourceLoading[source] = false
      if (this.live[source]?.requestId === requestId) {
        this.live[source] = undefined
      }
      this.publish()
    } catch (error) {
      if (!this.isLive(source, generation, sourceSequence, requestId)) return
      await this.handleSourceError(source, generation, live, error)
    }
  }

  private ingestPage(
    source: ResourceSource,
    generation: number,
    live: LiveSourceIdentity,
    pageIndex: number,
    page: ReferenceSearchPage
  ): boolean {
    if (
      page.sourceSequence !== live.sourceSequence ||
      page.requestId !== live.requestId ||
      page.pageIndex !== pageIndex
    ) {
      // Stale or malformed echo for a non-current identity is dropped; when
      // the identity is still current, treat malformed as protocol error.
      if (
        this.isLive(source, generation, live.sourceSequence, live.requestId)
      ) {
        this.markSourceProtocolError(source)
      }
      return false
    }

    if (!this.validatePageItems(source, page)) {
      this.markSourceProtocolError(source)
      return false
    }

    if (source === "file") {
      return this.ingestFilePage(generation, live, page, pageIndex)
    }
    if (source === "commit") {
      return this.ingestCommitPage(generation, live, page)
    }
    return this.ingestConversationPage(live, page)
  }

  private validatePageItems(
    source: ResourceSource,
    page: ReferenceSearchPage
  ): boolean {
    for (const item of page.items) {
      if (item.source !== source) return false
      if (
        item.metadata.kind !== source &&
        !(source === "conversation" && item.metadata.kind === "conversation")
      ) {
        return false
      }
      if (source === "file" && item.metadata.kind !== "file") return false
      if (source === "commit" && item.metadata.kind !== "commit") return false
      if (source === "conversation" && item.metadata.kind !== "conversation") {
        return false
      }
    }
    return true
  }

  private ingestFilePage(
    _generation: number,
    live: LiveSourceIdentity,
    page: ReferenceSearchPage,
    _pageIndex: number
  ): boolean {
    if (page.items.length === 0) {
      if (page.done && page.doneReason === "limit") {
        this.sourceTruncated.file = true
      }
      return true
    }

    let root: string | null = null
    for (const item of page.items) {
      if (item.metadata.kind !== "file") {
        this.markSourceProtocolError("file")
        return false
      }
      if (!item.metadata.canonicalWorkspaceRoot) {
        this.markSourceProtocolError("file")
        return false
      }
      if (root == null) root = item.metadata.canonicalWorkspaceRoot
      else if (root !== item.metadata.canonicalWorkspaceRoot) {
        this.markSourceProtocolError("file")
        return false
      }
    }
    if (!root || !this.defaultPath) {
      this.markSourceProtocolError("file")
      return false
    }

    // A later page after this drain already established a root must match it.
    // First non-empty page may still establish/repoint (even when pageIndex > 0
    // after empty leading pages, or when a provisional alias pointed elsewhere).
    const establishedRoot = live.fileCanonicalRoot
    if (establishedRoot != null && establishedRoot !== root) {
      this.markSourceProtocolError("file")
      return false
    }

    const previousBucket = this.fileBucket
    const newBucket: ReferenceCacheBucketKey = {
      backend: this.backendKey,
      source: "file",
      canonicalRoot: root,
    }

    if (establishedRoot == null) {
      // First non-empty page establishes/repoints the authoritative alias.
      live.fileCanonicalRoot = root
      this.cache.rememberFileRootAlias(this.backendKey, this.defaultPath, root)
      if (
        previousBucket &&
        serializeBucketKey(previousBucket) !== serializeBucketKey(newBucket)
      ) {
        this.discardRegexDrain("file")
        // Drop provisional rows from the old bucket membership.
        const map = this.membership.get("file")!
        for (const [uri, entry] of [...map.entries()]) {
          if (entry.bucketKey === serializeBucketKey(previousBucket)) {
            map.delete(uri)
          }
        }
      }
    }

    this.fileBucket = newBucket

    const accepted: ReferenceCandidate[] = []
    for (const candidate of page.items) {
      const merged = this.cache.mergeCandidate(newBucket, candidate)
      if (!merged) continue
      this.acceptMerged(
        "file",
        newBucket,
        merged.candidate,
        merged.mutationRevision
      )
      accepted.push(candidate)
    }

    // Regex drain is absent when there was no pre-existing alias at drain
    // start (or the provisional alias was discarded on repoint). Begin after
    // first merges so commitRegexRefresh snapshots match current revisions,
    // then collect ranks.
    if (isRegexQuery(this.query) && !this.regexDrains.file) {
      const handle = this.cache.beginRegexRefresh(
        this.searchSessionId,
        newBucket,
        this.query
      )
      this.regexDrains.file = { handle, items: [] }
    }
    for (const candidate of accepted) {
      this.noteRegexRank("file", candidate)
    }

    if (page.done && page.doneReason === "limit") {
      this.sourceTruncated.file = true
    }
    this.publish()
    return true
  }

  private ingestConversationPage(
    live: LiveSourceIdentity,
    page: ReferenceSearchPage
  ): boolean {
    const bucket = this.conversationBucket
    for (const candidate of page.items) {
      const merged = this.cache.mergeCandidate(bucket, candidate, {
        conversationPageStartedAt: live.conversationPageStartedAt,
      })
      if (!merged) continue
      this.acceptMerged(
        "session",
        bucket,
        merged.candidate,
        merged.mutationRevision
      )
      this.noteRegexRank("conversation", candidate)
    }
    if (page.done && page.doneReason === "limit") {
      this.sourceTruncated.conversation = true
    }
    this.publish()
    return true
  }

  private ingestCommitPage(
    generation: number,
    _live: LiveSourceIdentity,
    page: ReferenceSearchPage
  ): boolean {
    const head = this.inputs.gitHead
    const pageEpoch = page.sourceEpoch

    // Epoch must be non-empty and match current identity when known.
    if (!pageEpoch) {
      this.markSourceProtocolError("commit")
      return false
    }

    const currentEpoch = head?.reference_source_epoch ?? null
    const currentRepo = head?.canonical_repo ?? null

    if (
      currentEpoch == null ||
      currentRepo == null ||
      pageEpoch !== currentEpoch
    ) {
      // Discard page; guarded git refresh + restart.
      void this.enterGitRefreshGate(generation, head)
      return false
    }

    // Whole-page repository check before any merge.
    for (const item of page.items) {
      if (item.metadata.kind !== "commit") {
        this.markSourceProtocolError("commit")
        return false
      }
      if (item.metadata.canonicalRepo !== currentRepo) {
        this.markSourceProtocolError("commit")
        return false
      }
    }

    const bucket: ReferenceCacheBucketKey = {
      backend: this.backendKey,
      source: "commit",
      canonicalRepo: currentRepo,
      sourceEpoch: currentEpoch,
    }
    this.commitBucket = bucket

    for (const candidate of page.items) {
      const merged = this.cache.mergeCandidate(bucket, candidate)
      if (!merged) continue
      this.acceptMerged(
        "commit",
        bucket,
        merged.candidate,
        merged.mutationRevision
      )
      this.noteRegexRank("commit", candidate)
    }
    if (page.done && page.doneReason === "limit") {
      this.sourceTruncated.commit = true
    }
    this.publish()
    return true
  }

  private acceptMerged(
    group: "file" | "session" | "commit",
    bucket: ReferenceCacheBucketKey,
    candidate: ReferenceCandidate,
    mutationRevision: number
  ): void {
    if (this.query === "") return
    const map = this.membership.get(group)!
    const existing = map.get(candidate.uri)

    if (isRegexQuery(this.query)) {
      if (!candidate.regexRank && !existing?.item.regexRank) {
        // Page items should carry rank in regex mode; still accept metadata.
      }
      const item = candidateToSuggestion(candidate, "fresh", true)
      map.set(candidate.uri, {
        item,
        mutationRevision,
        bucketKey: serializeBucketKey(bucket),
        source: bucket.source,
        literalRank: null,
      })
      return
    }

    const fields = candidateSearchFields(candidate)
    const rank = rankLiteralFields(this.query, fields.primary, fields.secondary)
    if (rank == null) {
      // Live metadata no longer matches this literal query — drop membership.
      map.delete(candidate.uri)
      return
    }
    const item = candidateToSuggestion(candidate, "fresh", true)
    map.set(candidate.uri, {
      item,
      mutationRevision,
      bucketKey: serializeBucketKey(bucket),
      source: bucket.source,
      literalRank: rank,
    })
  }

  private noteRegexRank(
    source: ResourceSource,
    candidate: ReferenceCandidate
  ): void {
    const drain = this.regexDrains[source]
    if (!drain || !candidate.regexRank) return
    drain.items.push({ uri: candidate.uri, rank: candidate.regexRank })
  }

  private finishRegexDrain(source: ResourceSource): void {
    const drain = this.regexDrains[source]
    if (!drain) return
    this.cache.commitRegexRefresh(
      drain.handle,
      drain.items,
      this.sourceTruncated[source]
    )
    delete this.regexDrains[source]
  }

  private discardRegexDrain(source: ResourceSource): void {
    const drain = this.regexDrains[source]
    if (!drain) return
    this.cache.discardRegexRefresh(drain.handle)
    delete this.regexDrains[source]
  }

  private discardAllRegexDrains(): void {
    for (const source of [
      "file",
      "conversation",
      "commit",
    ] as ResourceSource[]) {
      this.discardRegexDrain(source)
    }
  }

  private markSourceProtocolError(source: ResourceSource): void {
    this.sourceError[source] = true
    this.sourceLoading[source] = false
    if (this.live[source]) this.live[source] = undefined
    this.discardRegexDrain(source)
    this.publish()
  }

  private async handleSourceError(
    source: ResourceSource,
    generation: number,
    live: LiveSourceIdentity,
    error: unknown
  ): Promise<void> {
    const code = errorCode(error)
    if (
      code === "cancelled" ||
      code === "stale_start" ||
      !this.isLive(source, generation, live.sourceSequence, live.requestId)
    ) {
      return
    }

    if (
      code === "job_expired" ||
      code === "stale_page" ||
      code === "limit_epoch_changed"
    ) {
      if (this.live[source]?.requestId === live.requestId) {
        this.live[source] = undefined
      }
      this.discardRegexDrain(source)
      void this.drainSource(source)
      return
    }

    if (code === "source_epoch_changed" && source === "commit") {
      if (this.live[source]?.requestId === live.requestId) {
        this.live[source] = undefined
      }
      this.discardRegexDrain(source)
      void this.enterGitRefreshGate(generation, this.inputs.gitHead)
      return
    }

    if (code === "invalid_pattern") {
      this.patternError = true
      this.sourceLoading[source] = false
      if (this.live[source]?.requestId === live.requestId) {
        this.live[source] = undefined
      }
      // Retain continuity rows non-selectable for this source group.
      const group = groupForSource(source)
      const map = this.membership.get(group)!
      for (const entry of map.values()) {
        entry.item = {
          ...entry.item,
          selectable: false,
          freshness: "cache",
        }
      }
      this.discardRegexDrain(source)
      this.publish()
      return
    }

    // timeout / overload / source failure / invalid_request → retain cache + error
    if (code === "invalid_request") {
      console.warn("reference search invalid_request", error)
    }
    this.sourceError[source] = true
    this.sourceLoading[source] = false
    if (this.live[source]?.requestId === live.requestId) {
      this.live[source] = undefined
    }
    this.discardRegexDrain(source)
    this.publish()
  }

  // ── Git identity ──────────────────────────────────────────────────────

  private handleGitHeadChange(
    prev: GitHeadInfo | null,
    next: GitHeadInfo | null
  ): void {
    this.clearGitRefreshGate()
    if (!this.active || this.query === "") {
      // Still adopt for later projections.
      if (this.active) {
        this.projectResourceCachesForQuery()
      }
      return
    }

    const wasRepo = prev?.is_repo === true
    const isRepo = next?.is_repo === true
    const wasUnknown = prev === null
    const becameNonRepo = next?.is_repo === false

    if (becameNonRepo) {
      this.guardedCancelSource("commit")
      this.discardRegexDrain("commit")
      this.membership.get("commit")!.clear()
      this.commitBucket = null
      this.sourceLoading.commit = false
      this.sourceError.commit = false
      this.sourceTruncated.commit = false
      return
    }

    // Identity change on same branch / epoch / repo → restart only commit.
    if (
      this.folderId != null &&
      this.defaultPath &&
      (isRepo || next === null)
    ) {
      const identityChanged =
        wasUnknown ||
        wasRepo !== isRepo ||
        commitIdentityKey(prev) !== commitIdentityKey(next)
      if (identityChanged) {
        this.guardedCancelSource("commit")
        this.discardRegexDrain("commit")
        this.membership.get("commit")!.clear()
        this.sourceError.commit = false
        this.sourceTruncated.commit = false
        this.projectResourceCachesForQuery()
        if (isRepo || next === null) {
          void this.drainSource("commit")
        }
      }
    }
  }

  private async enterGitRefreshGate(
    generation: number,
    oldHead: GitHeadInfo | null
  ): Promise<void> {
    if (!this.active || this.generation !== generation) return
    if (this.folderId == null || !this.defaultPath) return

    const oldKey = commitIdentityKey(oldHead)
    if (
      this.gitRefresh &&
      this.gitRefresh.generation === generation &&
      this.gitRefresh.oldCommitIdentity === oldKey
    ) {
      await this.gitRefresh.promise
      return
    }

    const promise = this.runGitRefresh(generation, oldKey)
    this.gitRefresh = { generation, oldCommitIdentity: oldKey, promise }
    try {
      await promise
    } finally {
      if (
        this.gitRefresh?.generation === generation &&
        this.gitRefresh.oldCommitIdentity === oldKey
      ) {
        this.gitRefresh = null
      }
    }
  }

  private async runGitRefresh(
    generation: number,
    oldKey: string
  ): Promise<void> {
    let head: GitHeadInfo
    try {
      head = await this.fetchGitHead()
    } catch {
      return
    }
    if (!this.active || this.generation !== generation) return
    if (commitIdentityKey(this.inputs.gitHead) !== oldKey) return

    this.applyGitHeadExternal(head)
    this.inputs = { ...this.inputs, gitHead: head }
    this.guardedCancelSource("commit")
    this.discardRegexDrain("commit")
    this.membership.get("commit")!.clear()
    this.sourceError.commit = false
    this.sourceTruncated.commit = false
    this.projectResourceCachesForQuery()
    if (this.folderId != null && this.defaultPath && (head.is_repo || true)) {
      // Restart commit so the new bucket is authoritative; skip when known non-repo.
      if (head.is_repo) {
        void this.drainSource("commit")
      } else {
        this.sourceLoading.commit = false
        this.publish()
      }
    }
  }

  private clearGitRefreshGate(): void {
    this.gitRefresh = null
  }

  // ── Validation ────────────────────────────────────────────────────────

  private beginValidation(
    uri: string,
    group: ReferenceGroupKind,
    entry: MembershipEntry
  ): Promise<ReferenceAttrs | null> {
    this.invalidateValidation()
    if (entry.mutationRevision == null || !entry.bucketKey) {
      return Promise.resolve(null)
    }
    const bucket = this.bucketForGroup(group)
    if (!bucket || bucket.source === undefined) {
      return Promise.resolve(null)
    }

    const validationRequestId = this.generateId()
    const generation = this.generation
    const mutationRevision = entry.mutationRevision
    const abort = new AbortController()

    // Mark validating.
    entry.item = { ...entry.item, freshness: "validating" }

    const promise = this.runValidation({
      validationRequestId,
      bucket,
      generation,
      selectedUri: uri,
      mutationRevision,
      abort,
    })

    this.validation = {
      validationRequestId,
      bucket,
      generation,
      selectedUri: uri,
      mutationRevision,
      abort,
      promise,
    }
    return promise
  }

  private async runValidation(state: {
    validationRequestId: string
    bucket: ReferenceCacheBucketKey
    generation: number
    selectedUri: string
    mutationRevision: number
    abort: AbortController
  }): Promise<ReferenceAttrs | null> {
    const source = state.bucket.source
    const req: ValidateReferenceCandidateRequest = {
      validationRequestId: state.validationRequestId,
      source,
      uri: state.selectedUri,
      query: this.query,
      workspacePath: source === "conversation" ? undefined : this.defaultPath,
      sourceEpoch:
        source === "commit" && state.bucket.source === "commit"
          ? state.bucket.sourceEpoch
          : undefined,
    }

    try {
      const result = await this.api.validate(req, state.abort.signal)
      return this.applyValidationResult(state, result)
    } catch (error) {
      return this.applyValidationError(state, error)
    }
  }

  private applyValidationResult(
    state: {
      validationRequestId: string
      bucket: ReferenceCacheBucketKey
      generation: number
      selectedUri: string
      mutationRevision: number
    },
    result: ReferenceCandidateValidation
  ): ReferenceAttrs | null {
    if (!this.validationIsCurrent(state, result.validationRequestId)) {
      return null
    }

    if (result.status === "not_found") {
      if (state.bucket.source === "conversation") {
        this.cache.markConversationNotFoundIfRevision(
          this.backendKey,
          state.selectedUri,
          state.mutationRevision
        )
      } else {
        this.cache.evictIfRevision(
          state.bucket,
          state.selectedUri,
          state.mutationRevision
        )
      }
      this.removeMembershipUri(state.selectedUri)
      this.clearValidationIf(state.validationRequestId)
      this.publish()
      return null
    }

    const candidate = result.candidate
    if (result.status === "match" || result.status === "not_match") {
      const merged = this.cache.mergeCandidate(state.bucket, {
        ...candidate,
        regexRank: result.regexRank ?? candidate.regexRank,
      })
      if (!merged) {
        this.removeMembershipUri(state.selectedUri)
        this.clearValidationIf(state.validationRequestId)
        this.publish()
        return null
      }

      if (result.status === "not_match") {
        // Merge live metadata, drop current-query membership only.
        this.removeMembershipUri(state.selectedUri)
        this.clearValidationIf(state.validationRequestId)
        this.publish()
        return null
      }

      // match
      const group = groupForSource(state.bucket.source)
      this.acceptMerged(
        group,
        state.bucket,
        merged.candidate,
        merged.mutationRevision
      )
      const entry = this.membership.get(group)!.get(state.selectedUri)
      if (entry) {
        entry.item = { ...entry.item, freshness: "fresh", selectable: true }
      }
      this.clearValidationIf(state.validationRequestId)
      this.publish()
      return entry ? { ...entry.item.reference } : null
    }

    return null
  }

  private async applyValidationError(
    state: {
      validationRequestId: string
      bucket: ReferenceCacheBucketKey
      generation: number
      selectedUri: string
      mutationRevision: number
    },
    error: unknown
  ): Promise<ReferenceAttrs | null> {
    if (!this.validationIsCurrent(state, state.validationRequestId)) {
      return null
    }
    const code = errorCode(error)
    if (
      code === "cancelled" ||
      code === "invalid_pattern" ||
      code === "invalid_request"
    ) {
      this.clearValidationIf(state.validationRequestId)
      this.clearValidatingMarks()
      this.publish()
      return null
    }

    if (code === "source_epoch_changed" && state.bucket.source === "commit") {
      this.clearValidationIf(state.validationRequestId)
      void this.enterGitRefreshGate(state.generation, this.inputs.gitHead)
      return null
    }

    // Operational: timeout / source failure / transport — return cached ref.
    this.clearValidationIf(state.validationRequestId)
    const located = this.locateMembership(state.selectedUri)
    if (!located) return null
    located.entry.item = {
      ...located.entry.item,
      freshness: "cache",
      selectable: true,
    }
    this.publish()
    return { ...located.entry.item.reference }
  }

  private validationIsCurrent(
    state: {
      validationRequestId: string
      generation: number
      selectedUri: string
      mutationRevision: number
    },
    echoedId: string
  ): boolean {
    if (!this.active) return false
    if (this.generation !== state.generation) return false
    if (this.selectedUri !== state.selectedUri) return false
    if (echoedId !== state.validationRequestId) return false
    if (
      !this.validation ||
      this.validation.validationRequestId !== state.validationRequestId
    ) {
      return false
    }
    const located = this.locateMembership(state.selectedUri)
    if (!located) return false
    if (located.entry.mutationRevision !== state.mutationRevision) return false
    return true
  }

  private invalidateValidation(): void {
    if (!this.validation) return
    this.validation.abort.abort()
    this.validation = null
  }

  private clearValidationIf(id: string): void {
    if (this.validation?.validationRequestId === id) {
      this.validation = null
    }
  }

  private clearValidatingMarks(): void {
    for (const map of this.membership.values()) {
      for (const entry of map.values()) {
        if (entry.item.freshness === "validating") {
          entry.item = { ...entry.item, freshness: "cache" }
        }
      }
    }
  }

  // ── Conversation cache events ─────────────────────────────────────────

  private acquireConversationSubscription(): void {
    if (this.conversationUnsub) return
    this.conversationUnsub = this.cache.subscribeConversationChanges(
      (change) => {
        this.onConversationCacheChange(change)
      }
    )
  }

  private releaseConversationSubscription(): void {
    if (!this.conversationUnsub) return
    this.conversationUnsub()
    this.conversationUnsub = null
  }

  private onConversationCacheChange(
    change: ReferenceConversationCacheChange
  ): void {
    if (!this.active) return
    if (change.backend !== this.backendKey) return

    const map = this.membership.get("session")!
    if (change.kind === "delete") {
      map.delete(change.uri)
      if (this.selectedUri === change.uri) {
        this.selectedUri = null
      }
      this.publish()
      return
    }

    const entry = map.get(change.uri)
    if (!entry) return

    if (change.kind === "upsert") {
      // Update from carried summary while preserving project metadata.
      const ref = entry.item.reference
      const label =
        // summary title folding is done by the cache; use summary fields for display
        change.summary
          ? // Prefer cache-updated candidate if revision present via re-read
            entry.item.reference.label
          : entry.item.reference.label
      void label
      // Patch item from summary fields.
      const newLabel = entry.item.reference.label
      // Use summary-derived fields when we only have the event (revision may be null).
      const summaryLabel = formatSummaryLabel(change.summary)
      const branch = change.summary.git_branch
      const status = change.summary.status
      const nextItem: SuggestionItem = {
        ...entry.item,
        reference: {
          ...ref,
          label: summaryLabel,
          meta: {
            ...ref.meta,
            agentType: change.summary.agent_type,
            status,
            branch,
          },
        },
        detail: branch ?? status,
        keywords: `${summaryLabel} ${change.summary.agent_type}`,
        freshness: "cache",
      }
      void newLabel
      entry.item = nextItem
      if (change.mutationRevision != null) {
        entry.mutationRevision = change.mutationRevision
      }

      if (isRegexQuery(this.query)) {
        if (this.selectedUri === change.uri) {
          entry.item = { ...entry.item, freshness: "cache", selectable: true }
          void this.beginValidation(change.uri, "session", entry)
        } else {
          map.delete(change.uri)
        }
      } else if (this.query !== "") {
        const fields = {
          primary: [entry.item.reference.label],
          secondary: [
            entry.item.reference.id,
            change.summary.agent_type,
            status,
            branch ?? "",
            "",
            "",
          ],
        }
        const rank = rankLiteralFields(
          this.query,
          fields.primary,
          fields.secondary
        )
        if (rank == null) {
          map.delete(change.uri)
        } else {
          entry.literalRank = rank
        }
      }
      this.publish()
      return
    }

    if (change.kind === "status") {
      const ref = entry.item.reference
      const branch = ref.meta?.branch ?? null
      entry.item = {
        ...entry.item,
        reference: {
          ...ref,
          meta: {
            ...ref.meta,
            status: change.status,
          },
        },
        detail: branch == null ? change.status : entry.item.detail,
        freshness: "cache",
      }
      if (change.mutationRevision != null) {
        entry.mutationRevision = change.mutationRevision
      }
      if (isRegexQuery(this.query)) {
        if (this.selectedUri === change.uri) {
          void this.beginValidation(change.uri, "session", entry)
        } else {
          map.delete(change.uri)
        }
      } else if (this.query !== "") {
        const fields = {
          primary: [entry.item.reference.label],
          secondary: [
            entry.item.reference.id,
            ref.meta?.agentType ?? "",
            change.status,
            branch ?? "",
            "",
            "",
          ],
        }
        const rank = rankLiteralFields(
          this.query,
          fields.primary,
          fields.secondary
        )
        if (rank == null) map.delete(change.uri)
        else entry.literalRank = rank
      }
      this.publish()
    }
  }

  // ── Snapshot / pins ───────────────────────────────────────────────────

  private publish(): void {
    this.snapshot = this.buildSnapshot()
    this.syncVisiblePins()
    for (const listener of this.listeners) {
      try {
        listener()
      } catch {
        // Isolate listener failures.
      }
    }
  }

  private buildSnapshot(): ReferenceSearchSnapshot {
    const labels = this.inputs.labels
    const regex = isRegexQuery(this.query) && this.query !== ""
    return {
      query: this.query,
      generation: this.generation,
      patternError: this.patternError,
      groups: {
        agent: this.snapshotGroup(
          "agent",
          labels.agent,
          this.agentLoading,
          this.agentTruncated,
          this.agentError,
          regex
        ),
        file: this.snapshotGroup(
          "file",
          labels.file,
          this.sourceLoading.file,
          this.sourceTruncated.file,
          this.sourceError.file ? "source" : null,
          regex
        ),
        session: this.snapshotGroup(
          "session",
          labels.session,
          this.sourceLoading.conversation,
          this.sourceTruncated.conversation,
          this.sourceError.conversation ? "source" : null,
          regex
        ),
        commit: this.snapshotGroup(
          "commit",
          labels.commit,
          this.sourceLoading.commit,
          this.sourceTruncated.commit,
          this.sourceError.commit ? "source" : null,
          regex
        ),
      },
    }
  }

  private snapshotGroup(
    kind: ReferenceGroupKind,
    label: string,
    loading: boolean,
    truncated: boolean,
    error: "profile" | "source" | null,
    regex: boolean
  ): ReferenceGroupSnapshot {
    const map = this.membership.get(kind)!
    const rows = [...map.values()]
    if (regex) {
      rows.sort((a, b) =>
        compareRegex(
          {
            rank: a.item.regexRank,
            ordinal: a.item.sourceOrdinal,
            uri: a.item.reference.uri,
          },
          {
            rank: b.item.regexRank,
            ordinal: b.item.sourceOrdinal,
            uri: b.item.reference.uri,
          }
        )
      )
    } else {
      rows.sort((a, b) =>
        compareLiteral(
          {
            rank: a.literalRank,
            ordinal: a.item.sourceOrdinal,
            uri: a.item.reference.uri,
          },
          {
            rank: b.literalRank,
            ordinal: b.item.sourceOrdinal,
            uri: b.item.reference.uri,
          }
        )
      )
    }

    const limit =
      kind === "agent" ? Number.POSITIVE_INFINITY : this.inputs.referenceLimit
    // When rank truncation would hide the selected URI, reserve one visible
    // slot for it and drop the lowest-ranked unselected item.
    let chosen = rows
    if (
      Number.isFinite(limit) &&
      rows.length > limit &&
      this.selectedUri != null
    ) {
      const selectedAt = rows.findIndex(
        (r) => r.item.reference.uri === this.selectedUri
      )
      if (selectedAt >= limit) {
        chosen = [...rows.slice(0, limit - 1), rows[selectedAt]]
      } else {
        chosen = rows.slice(0, limit)
      }
    } else {
      chosen = rows.slice(0, limit)
    }
    const sliced = chosen.map((r) => r.item)
    const localTruncated =
      kind !== "agent" && rows.length > this.inputs.referenceLimit
    return {
      kind,
      label,
      items: sliced,
      loading,
      truncated: truncated || localTruncated,
      error,
    }
  }

  private syncVisiblePins(): void {
    if (!this.active) return
    const next = new Map<
      string,
      { bucket: ReferenceCacheBucketKey; uris: string[] }
    >()

    const fileMap = this.membership.get("file")!
    if (this.fileBucket && fileMap.size > 0) {
      next.set(serializeBucketKey(this.fileBucket), {
        bucket: this.fileBucket,
        uris: [...fileMap.keys()],
      })
    }
    const sessionMap = this.membership.get("session")!
    if (sessionMap.size > 0) {
      next.set(serializeBucketKey(this.conversationBucket), {
        bucket: this.conversationBucket,
        uris: [...sessionMap.keys()],
      })
    }
    const commitMap = this.membership.get("commit")!
    if (this.commitBucket && commitMap.size > 0) {
      next.set(serializeBucketKey(this.commitBucket), {
        bucket: this.commitBucket,
        uris: [...commitMap.keys()],
      })
    }

    for (const [key, bucket] of this.pinnedBuckets) {
      if (!next.has(key)) {
        // Clear pins for abandoned file/commit buckets after alias repoint
        // or epoch switch (keys no longer reconstructible from live buckets).
        this.cache.pinVisible(this.searchSessionId, bucket, [])
      }
    }
    for (const { bucket, uris } of next.values()) {
      this.cache.pinVisible(this.searchSessionId, bucket, uris)
    }
    this.pinnedBuckets = new Map(
      [...next.entries()].map(([key, value]) => [key, value.bucket])
    )
  }

  // ── Helpers ───────────────────────────────────────────────────────────

  private nextSequence(source: ResourceSource): number {
    const next = this.sequences[source] + 1
    if (!Number.isSafeInteger(next) || next <= 0) {
      throw new Error(`source sequence overflow for ${source}`)
    }
    this.sequences[source] = next
    return next
  }

  private isLive(
    source: ResourceSource,
    generation: number,
    sourceSequence: number,
    requestId: string
  ): boolean {
    if (!this.active || this.generation !== generation) return false
    const live = this.live[source]
    return (
      live != null &&
      live.sourceSequence === sourceSequence &&
      live.requestId === requestId
    )
  }

  private collectedCount(source: ResourceSource): number {
    return this.membership.get(groupForSource(source))!.size
  }

  private guardedCancelAllSources(): void {
    for (const source of [
      "file",
      "conversation",
      "commit",
    ] as ResourceSource[]) {
      this.guardedCancelSource(source)
    }
  }

  private guardedCancelSource(source: ResourceSource): void {
    const live = this.live[source]
    if (!live) {
      this.sourceLoading[source] = false
      return
    }
    live.abort.abort()
    void this.api
      .cancel({
        searchSessionId: this.searchSessionId,
        sourceSequence: live.sourceSequence,
        requestId: live.requestId,
        source,
      })
      .catch(() => undefined)
    this.live[source] = undefined
    this.sourceLoading[source] = false
  }

  private bucketForSource(
    source: ResourceSource
  ): ReferenceCacheBucketKey | null {
    if (source === "file") return this.fileBucket
    if (source === "conversation") return this.conversationBucket
    return this.commitBucket
  }

  private bucketForGroup(
    group: ReferenceGroupKind
  ): ReferenceCacheBucketKey | null {
    if (group === "file") return this.fileBucket
    if (group === "session") return this.conversationBucket
    if (group === "commit") return this.commitBucket
    return null
  }

  private locateMembership(
    uri: string
  ): { group: ReferenceGroupKind; entry: MembershipEntry } | null {
    for (const kind of [
      "agent",
      "file",
      "session",
      "commit",
    ] as ReferenceGroupKind[]) {
      const entry = this.membership.get(kind)!.get(uri)
      if (entry) return { group: kind, entry }
    }
    return null
  }

  private removeMembershipUri(uri: string): void {
    for (const map of this.membership.values()) {
      map.delete(uri)
    }
  }

  private resetResourceMembership(): void {
    this.membership.get("file")!.clear()
    this.membership.get("session")!.clear()
    this.membership.get("commit")!.clear()
  }

  private resetAllMembership(): void {
    for (const map of this.membership.values()) map.clear()
  }

  private buildEmptySnapshot(): ReferenceSearchSnapshot {
    const labels = this.inputs.labels
    return {
      query: "",
      generation: 0,
      patternError: false,
      groups: {
        agent: emptyGroup("agent", labels.agent),
        file: emptyGroup("file", labels.file),
        session: emptyGroup("session", labels.session),
        commit: emptyGroup("commit", labels.commit),
      },
    }
  }

  private async withTimeout<T>(promise: Promise<T>, ms: number): Promise<T> {
    let timer: ReturnType<typeof setTimeout> | undefined
    try {
      return await Promise.race([
        promise,
        new Promise<T>((_, reject) => {
          timer = setTimeout(() => reject(new Error("confirm timeout")), ms)
        }),
      ])
    } catch {
      // On timeout fall through to cached reference if still present.
      const uri = this.validation?.selectedUri
      if (uri) {
        const located = this.locateMembership(uri)
        if (located) return { ...located.entry.item.reference } as T
      }
      return null as T
    } finally {
      if (timer) clearTimeout(timer)
    }
  }
}

function groupForSource(source: ResourceSource): "file" | "session" | "commit" {
  if (source === "conversation") return "session"
  return source
}

function sameAgents(a: AcpAgentInfo[], b: AcpAgentInfo[]): boolean {
  if (a === b) return true
  if (a.length !== b.length) return false
  for (let i = 0; i < a.length; i++) {
    const x = a[i]!
    const y = b[i]!
    if (
      x.agent_type !== y.agent_type ||
      x.enabled !== y.enabled ||
      x.available !== y.available ||
      x.name !== y.name ||
      x.description !== y.description ||
      x.sort_order !== y.sort_order
    ) {
      return false
    }
  }
  return true
}

function sameCatalog(
  a: DelegationProfileCatalog | null,
  b: DelegationProfileCatalog | null
): boolean {
  if (a === b) return true
  if (!a || !b) return false
  return (
    a.revision === b.revision &&
    a.delegation_enabled === b.delegation_enabled &&
    a.profiles === b.profiles
  )
}

function sameLabels(a: ReferenceGroupLabels, b: ReferenceGroupLabels): boolean {
  return (
    a.agent === b.agent &&
    a.file === b.file &&
    a.session === b.session &&
    a.commit === b.commit &&
    a.skill === b.skill
  )
}

function sameGitHead(a: GitHeadInfo | null, b: GitHeadInfo | null): boolean {
  if (a === b) return true
  if (!a || !b) return false
  return (
    a.is_repo === b.is_repo &&
    a.branch === b.branch &&
    a.detached === b.detached &&
    a.short_sha === b.short_sha &&
    a.canonical_repo === b.canonical_repo &&
    a.head_sha === b.head_sha &&
    a.reference_source_epoch === b.reference_source_epoch
  )
}

function formatSummaryLabel(summary: {
  id: number
  title: string | null
}): string {
  const folded = formatConversationTitle(summary.title).trim()
  return folded || `#${summary.id}`
}

async function runWithConcurrency<T>(
  items: T[],
  limit: number,
  worker: (item: T) => Promise<void>
): Promise<void> {
  if (items.length === 0) return
  let index = 0
  let firstError: unknown = null
  const run = async () => {
    while (index < items.length) {
      if (firstError) return
      const current = items[index++]!
      try {
        await worker(current)
      } catch (error) {
        firstError = error
        return
      }
    }
  }
  const runners = Array.from({ length: Math.min(limit, items.length) }, () =>
    run()
  )
  await Promise.all(runners)
  if (firstError) throw firstError
}

// Re-export labels type surface used by the hook module.
export type { ReferenceGroupLabels }
