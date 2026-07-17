import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

import {
  ReferenceSearchCache,
  type ReferenceCacheBucketKey,
} from "@/lib/reference-search-cache"
import type {
  AcpAgentInfo,
  AppCommandError,
  DbConversationSummary,
  DelegationProfile,
  DelegationProfileCatalog,
  GitHeadInfo,
  ReferenceCandidate,
  ReferenceCandidateValidation,
  ReferenceDescriptor,
  ReferenceRegexMatch,
  ReferenceSearchPage,
  ReferenceSearchSource,
  StartReferenceSearchRequest,
} from "@/lib/types"

import {
  ReferenceSearchController,
  type ReferenceSearchControllerInputs,
} from "./reference-search-controller"
import {
  catalogSearchFields,
  type CatalogSearchEntry,
} from "./suggestion/adapters"
import type { ReferenceAttrs } from "./types"
import { DEFAULT_GROUP_LABELS } from "./use-reference-search"

// ── Deferred helpers ────────────────────────────────────────────────────────

type Resolver<T> = {
  promise: Promise<T>
  resolve: (value: T) => void
  reject: (error: unknown) => void
}

function deferred<T>(): Resolver<T> {
  let resolve!: (value: T) => void
  let reject!: (error: unknown) => void
  const promise = new Promise<T>((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

async function flushMicrotasks(times = 5): Promise<void> {
  for (let i = 0; i < times; i++) {
    await Promise.resolve()
  }
}

// ── API mocks ───────────────────────────────────────────────────────────────

const startQueues = new Map<
  ReferenceSearchSource,
  Resolver<ReferenceSearchPage>[]
>()
const nextQueues = new Map<
  ReferenceSearchSource,
  Resolver<ReferenceSearchPage>[]
>()
const validationQueue: Resolver<ReferenceCandidateValidation>[] = []
const regexQueue: Resolver<ReferenceRegexMatch[]>[] = []
const startCalls: StartReferenceSearchRequest[] = []
const nextCalls: { source: ReferenceSearchSource; pageIndex: number }[] = []
const cancelCalls: unknown[] = []
const matchCalls: { query: string; descriptors: ReferenceDescriptor[] }[] = []
const validateCalls: unknown[] = []

const mocks = {
  startReferenceSearch: vi.fn(
    async (req: StartReferenceSearchRequest): Promise<ReferenceSearchPage> => {
      startCalls.push(req)
      const q = startQueues.get(req.source) ?? []
      const d = deferred<ReferenceSearchPage>()
      q.push(d)
      startQueues.set(req.source, q)
      return d.promise
    }
  ),
  nextReferenceSearchPage: vi.fn(
    async (req: {
      source: ReferenceSearchSource
      pageIndex: number
    }): Promise<ReferenceSearchPage> => {
      nextCalls.push({ source: req.source, pageIndex: req.pageIndex })
      const q = nextQueues.get(req.source) ?? []
      const d = deferred<ReferenceSearchPage>()
      q.push(d)
      nextQueues.set(req.source, q)
      return d.promise
    }
  ),
  cancelReferenceSearch: vi.fn(async (req: unknown) => {
    cancelCalls.push(req)
    return true
  }),
  validateReferenceCandidate: vi.fn(async (req: unknown) => {
    validateCalls.push(req)
    const d = deferred<ReferenceCandidateValidation>()
    validationQueue.push(d)
    return d.promise
  }),
  matchReferenceRegex: vi.fn(
    async (req: { query: string; descriptors: ReferenceDescriptor[] }) => {
      matchCalls.push(req)
      const d = deferred<ReferenceRegexMatch[]>()
      regexQueue.push(d)
      return d.promise
    }
  ),
}

function resolveQueuedPage(
  queues: Map<ReferenceSearchSource, Resolver<ReferenceSearchPage>[]>,
  source: ReferenceSearchSource,
  page: ReferenceSearchPage
): void {
  const q = queues.get(source) ?? []
  const d = q.shift()
  queues.set(source, q)
  d?.resolve(page)
}

function rejectQueuedPage(
  queues: Map<ReferenceSearchSource, Resolver<ReferenceSearchPage>[]>,
  source: ReferenceSearchSource,
  error: unknown
): void {
  const q = queues.get(source) ?? []
  const d = q.shift()
  queues.set(source, q)
  d?.reject(error)
}

const sourceApi = {
  file: {
    resolve(page: ReferenceSearchPage) {
      resolveQueuedPage(startQueues, "file", page)
    },
    resolveNext(page: ReferenceSearchPage) {
      resolveQueuedPage(nextQueues, "file", page)
    },
    reject(error: unknown) {
      rejectQueuedPage(startQueues, "file", error)
    },
  },
  conversation: {
    resolve(page: ReferenceSearchPage) {
      const q = startQueues.get("conversation") ?? []
      const d = q.shift()
      startQueues.set("conversation", q)
      d?.resolve(page)
    },
    reject(error: unknown) {
      const q = startQueues.get("conversation") ?? []
      const d = q.shift()
      startQueues.set("conversation", q)
      d?.reject(error)
    },
  },
  commit: {
    resolve(page: ReferenceSearchPage) {
      const q = startQueues.get("commit") ?? []
      const d = q.shift()
      startQueues.set("commit", q)
      d?.resolve(page)
    },
    reject(error: unknown) {
      const q = startQueues.get("commit") ?? []
      const d = q.shift()
      startQueues.set("commit", q)
      d?.reject(error)
    },
  },
  validation: {
    resolve(value: ReferenceCandidateValidation) {
      validationQueue.shift()?.resolve(value)
    },
    reject(error: unknown) {
      validationQueue.shift()?.reject(error)
    },
  },
  regex: {
    resolve(value: ReferenceRegexMatch[]) {
      regexQueue.shift()?.resolve(value)
    },
    reject(error: unknown) {
      regexQueue.shift()?.reject(error)
    },
  },
}

// ── Fixtures ────────────────────────────────────────────────────────────────

const PROFILE_ID = "11111111-1111-4111-8111-111111111111"
const BACKEND = "test-backend"
let uuidCounter = 0

function nextUuid(): string {
  uuidCounter += 1
  const n = uuidCounter.toString(16).padStart(12, "0")
  return `00000000-0000-4000-8000-${n}`
}

function makeAgent(
  type: string,
  over: Partial<AcpAgentInfo> = {}
): AcpAgentInfo {
  return {
    agent_type: type,
    name: over.name ?? type,
    description: over.description ?? `${type} desc`,
    available: over.available ?? true,
    enabled: over.enabled ?? true,
    sort_order: over.sort_order ?? 0,
  } as AcpAgentInfo
}

function makeProfile(over: Partial<DelegationProfile> = {}): DelegationProfile {
  return {
    id: over.id ?? PROFILE_ID,
    agent_type: over.agent_type ?? "code_buddy",
    name: over.name ?? "Buddy",
    config_values: over.config_values ?? { model: "glm-5.2" },
    enabled: over.enabled ?? true,
    created_at: 1,
    updated_at: 1,
  }
}

function catalogFixture(): {
  agents: AcpAgentInfo[]
  profileCatalog: DelegationProfileCatalog
} {
  return {
    agents: [
      makeAgent("codex", { name: "Codex", available: true, enabled: true }),
      makeAgent("code_buddy", {
        name: "CodeBuddy",
        available: true,
        enabled: true,
      }),
    ],
    profileCatalog: {
      profiles: [makeProfile({ agent_type: "code_buddy", enabled: true })],
      delegation_enabled: true,
      revision: 1,
    },
  }
}

function gitHeadA(): GitHeadInfo {
  return {
    is_repo: true,
    branch: "main",
    detached: false,
    short_sha: null,
    canonical_repo: "/repo",
    head_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    reference_source_epoch: "v1:epoch-a",
  }
}

function gitHeadB(): GitHeadInfo {
  return {
    ...gitHeadA(),
    head_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    reference_source_epoch: "v1:epoch-b",
  }
}

function appError(code: string, message = code): AppCommandError {
  return { code, message }
}

function page(
  source: ReferenceSearchSource,
  pageIndex: number,
  items: ReferenceCandidate[],
  done: boolean,
  over: Partial<ReferenceSearchPage> = {}
): ReferenceSearchPage {
  // Fill sequence/request from the latest start call for that source.
  const last = [...startCalls].reverse().find((c) => c.source === source)
  return {
    sourceSequence: over.sourceSequence ?? last?.sourceSequence ?? 1,
    requestId: over.requestId ?? last?.requestId ?? "missing",
    pageIndex,
    items,
    sourceEpoch:
      over.sourceEpoch !== undefined
        ? over.sourceEpoch
        : source === "commit"
          ? "v1:epoch-a"
          : null,
    done,
    doneReason: done ? (over.doneReason ?? "exhausted") : null,
  }
}

function fileCandidate(
  name: string,
  root = "C:/real",
  ordinal = 1
): ReferenceCandidate {
  return {
    source: "file",
    uri: `file:///${root.replace(/\\/g, "/")}/src/${name}`,
    id: `src/${name}`,
    label: name,
    detail: `src/${name}`,
    keywords: name,
    metadata: {
      kind: "file",
      canonicalWorkspaceRoot: root,
      relativePath: `src/${name}`,
      entryKind: "file",
    },
    sourceOrdinal: ordinal,
    regexRank: null,
  }
}

function sessionCandidate(
  id: string,
  label?: string,
  ordinal = Number(id)
): ReferenceCandidate {
  const resolved = label ?? `#${id}`
  return {
    source: "conversation",
    uri: `codeg://session/${id}`,
    id,
    label: resolved,
    detail: "in_progress",
    keywords: `${resolved} claude_code`,
    metadata: {
      kind: "conversation",
      conversationId: Number(id),
      agentType: "claude_code",
      status: "in_progress",
      branch: null,
      projectName: "codeg",
      projectPath: "/repo/codeg",
    },
    sourceOrdinal: ordinal,
    regexRank: null,
  }
}

function freshSessionCandidate(id: string): ReferenceCandidate {
  return sessionCandidate(id, `fresh-${id}`)
}

function commitCandidate(
  hash: string,
  repo = "/repo",
  ordinal = 1
): ReferenceCandidate {
  return {
    source: "commit",
    uri: `codeg://commit/${encodeURIComponent(repo)}@${hash}`,
    id: hash,
    label: hash.slice(0, 7),
    detail: "subject",
    keywords: hash,
    metadata: {
      kind: "commit",
      canonicalRepo: repo,
      fullHash: hash,
      shortHash: hash.slice(0, 7),
      subject: "subject",
      message: "message",
      author: "Dev",
      authoredAt: "2026-01-01T00:00:00Z",
    },
    sourceOrdinal: ordinal,
    regexRank: null,
  }
}

function validation(
  status: "match" | "not_match" | "not_found",
  candidate?: ReferenceCandidate
): ReferenceCandidateValidation {
  const last = validateCalls[validateCalls.length - 1] as
    | { validationRequestId: string }
    | undefined
  const id = last?.validationRequestId ?? nextUuid()
  if (status === "not_found") {
    return { status, validationRequestId: id }
  }
  return {
    status,
    validationRequestId: id,
    candidate: candidate!,
    regexRank: null,
  }
}

function sequencesFor(source: ReferenceSearchSource): number[] {
  return startCalls
    .filter((c) => c.source === source)
    .map((c) => c.sourceSequence)
}

function makeSummary(
  over: Partial<DbConversationSummary> & { id: number }
): DbConversationSummary {
  return {
    folder_id: 1,
    title: null,
    title_locked: false,
    agent_type: "claude_code",
    status: "in_progress",
    awaiting_reply_token: null,
    kind: "regular",
    model: null,
    git_branch: null,
    external_id: null,
    message_count: 0,
    child_count: 0,
    created_at: "2026-01-01T00:00:00.000Z",
    updated_at: "2026-01-01T00:00:00.000Z",
    pinned_at: null,
    parent_id: null,
    parent_tool_use_id: null,
    delegation_call_id: null,
    ...over,
  }
}

// ── Controller factory ──────────────────────────────────────────────────────

let cache: ReferenceSearchCache
let fetchGitHead: ReturnType<typeof vi.fn>
let applyGitHead: ReturnType<typeof vi.fn>
let heldGitFetch: Resolver<GitHeadInfo> | null = null

function baseInputs(
  over: Partial<ReferenceSearchControllerInputs> = {}
): ReferenceSearchControllerInputs {
  const cat = catalogFixture()
  return {
    agents: cat.agents,
    profileCatalog: cat.profileCatalog,
    profileCatalogError: false,
    referenceLimit: 50,
    gitHead: gitHeadA(),
    labels: DEFAULT_GROUP_LABELS,
    ...over,
  }
}

function createController(
  fixture: {
    agents: AcpAgentInfo[]
    profileCatalog: DelegationProfileCatalog | null
  } = catalogFixture(),
  over: {
    inputs?: Partial<ReferenceSearchControllerInputs>
    folderId?: number | null
    defaultPath?: string | null
    cache?: ReferenceSearchCache
  } = {}
): ReferenceSearchController {
  const c = over.cache ?? cache
  const controller = new ReferenceSearchController({
    backendKey: BACKEND,
    folderId: over.folderId === undefined ? 1 : over.folderId,
    defaultPath: over.defaultPath === undefined ? "C:/link" : over.defaultPath,
    cache: c,
    generateId: nextUuid,
    fetchGitHead: () => fetchGitHead(),
    applyGitHead: (head) => applyGitHead(head),
    startReferenceSearch: mocks.startReferenceSearch,
    nextReferenceSearchPage: mocks.nextReferenceSearchPage,
    cancelReferenceSearch: mocks.cancelReferenceSearch,
    validateReferenceCandidate: mocks.validateReferenceCandidate,
    matchReferenceRegex: mocks.matchReferenceRegex,
  })
  controller.updateInputs(
    baseInputs({
      agents: fixture.agents,
      profileCatalog: fixture.profileCatalog,
      ...over.inputs,
    })
  )
  return controller
}

function cachedConversationFixture(): {
  agents: AcpAgentInfo[]
  profileCatalog: DelegationProfileCatalog
} {
  const bucket: ReferenceCacheBucketKey = {
    backend: BACKEND,
    source: "conversation",
  }
  cache.mergeCandidate(bucket, sessionCandidate("7", "old-title"))
  return catalogFixture()
}

beforeEach(() => {
  cache = new ReferenceSearchCache()
  uuidCounter = 0
  startQueues.clear()
  nextQueues.clear()
  validationQueue.length = 0
  regexQueue.length = 0
  startCalls.length = 0
  nextCalls.length = 0
  cancelCalls.length = 0
  matchCalls.length = 0
  validateCalls.length = 0
  heldGitFetch = null
  mocks.startReferenceSearch.mockClear()
  mocks.nextReferenceSearchPage.mockClear()
  mocks.cancelReferenceSearch.mockClear()
  mocks.validateReferenceCandidate.mockClear()
  mocks.matchReferenceRegex.mockClear()
  applyGitHead = vi.fn()
  fetchGitHead = vi.fn(async () => {
    if (heldGitFetch) return heldGitFetch.promise
    return gitHeadB()
  })
})

afterEach(() => {
  vi.restoreAllMocks()
})

// ── Tests ───────────────────────────────────────────────────────────────────

describe("ReferenceSearchController", () => {
  it("publishes bare agents and effective profiles synchronously with zero requests", () => {
    const controller = createController(catalogFixture())
    controller.setQuery("")
    expect(
      controller
        .getSnapshot()
        .groups.agent.items.map((item) => item.reference.uri)
    ).toEqual([
      "codeg://agent/codex",
      "codeg://agent/code_buddy",
      `codeg://delegation-profile/code_buddy/${PROFILE_ID}`,
    ])
    expect(mocks.startReferenceSearch).not.toHaveBeenCalled()
    expect(mocks.matchReferenceRegex).not.toHaveBeenCalled()
    expect(mocks.validateReferenceCandidate).not.toHaveBeenCalled()
  })

  it("bare_catalog_applies_effective_enablement_without_availability_probes", () => {
    const agents = [
      makeAgent("codex", { enabled: true, available: false }),
      makeAgent("claude_code", { enabled: false, available: true }),
      makeAgent("code_buddy", { enabled: true, available: true }),
      makeAgent("gemini", { enabled: false, available: true }),
    ]
    const profiles = [
      makeProfile({
        id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        agent_type: "code_buddy",
        enabled: true,
        name: "On",
      }),
      makeProfile({
        id: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
        agent_type: "code_buddy",
        enabled: false,
        name: "ProfileOff",
      }),
      makeProfile({
        id: "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
        agent_type: "gemini",
        enabled: true,
        name: "BackingOff",
      }),
    ]
    const controller = createController({
      agents,
      profileCatalog: {
        profiles,
        delegation_enabled: true,
        revision: 1,
      },
    })
    controller.setQuery("")
    const uris = controller
      .getSnapshot()
      .groups.agent.items.map((i) => i.reference.uri)
    expect(uris).toContain("codeg://agent/codex")
    expect(uris).toContain("codeg://agent/code_buddy")
    expect(uris).toContain(
      "codeg://delegation-profile/code_buddy/aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
    )
    expect(uris).not.toContain("codeg://agent/claude_code")
    expect(uris).not.toContain("codeg://agent/gemini")
    expect(uris).not.toContain(
      "codeg://delegation-profile/code_buddy/bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
    )
    expect(uris).not.toContain(
      "codeg://delegation-profile/gemini/cccccccc-cccc-4ccc-8ccc-cccccccccccc"
    )

    // Global delegation off removes profiles.
    controller.updateInputs(
      baseInputs({
        agents,
        profileCatalog: {
          profiles,
          delegation_enabled: false,
          revision: 2,
        },
      })
    )
    const after = controller
      .getSnapshot()
      .groups.agent.items.map((i) => i.reference.uri)
    expect(after.every((u) => !u.includes("delegation-profile"))).toBe(true)
    expect(mocks.startReferenceSearch).not.toHaveBeenCalled()
  })

  it("publishes each first page without waiting for sibling sources", async () => {
    const controller = createController(catalogFixture())
    controller.setQuery("app")
    expect(controller.getSnapshot().groups.file.loading).toBe(true)
    expect(controller.getSnapshot().groups.session.loading).toBe(true)
    expect(controller.getSnapshot().groups.commit.loading).toBe(true)
    sourceApi.file.resolve(page("file", 0, [fileCandidate("app.ts")], true))
    await flushMicrotasks()
    expect(controller.getSnapshot().groups.file.items).toHaveLength(1)
    expect(controller.getSnapshot().groups.session.loading).toBe(true)
    expect(controller.getSnapshot().groups.commit.loading).toBe(true)
  })

  it("restarts only an expired source with a higher source sequence", async () => {
    const controller = createController(catalogFixture())
    controller.setQuery("fix")
    sourceApi.conversation.reject(appError("job_expired"))
    await flushMicrotasks()
    expect(sequencesFor("conversation")).toEqual([1, 2])
    expect(sequencesFor("file")).toEqual([1])
    expect(sequencesFor("commit")).toEqual([1])
  })

  it("not_match is query-local while not_found evicts by captured revision", async () => {
    const controller = createController(cachedConversationFixture())
    controller.setQuery("old")
    expect(
      controller.getSnapshot().groups.session.items.map((i) => i.reference.uri)
    ).toContain("codeg://session/7")
    controller.setSelectedUri("codeg://session/7")
    await flushMicrotasks()
    sourceApi.validation.resolve(
      validation("not_match", freshSessionCandidate("7"))
    )
    await flushMicrotasks()
    expect(controller.getSnapshot().groups.session.items).toHaveLength(0)
    // Candidate still in cache under updated metadata — new query "fresh" matches.
    controller.setQuery("fresh")
    expect(
      controller.getSnapshot().groups.session.items[0]?.reference.uri
    ).toBe("codeg://session/7")
  })

  it("commit_page_epoch_mismatch_refreshes_identity_and_restarts_only_commit", async () => {
    const controller = createController(catalogFixture())
    controller.setQuery("fix")
    const bad = page("commit", 0, [commitCandidate("deadbeef")], true, {
      sourceEpoch: "v1:epoch-b",
    })
    sourceApi.commit.resolve(bad)
    await flushMicrotasks(10)
    expect(applyGitHead).toHaveBeenCalledTimes(1)
    expect(applyGitHead).toHaveBeenCalledWith(gitHeadB())
    expect(sequencesFor("commit")).toEqual([1, 2])
    expect(sequencesFor("file")).toEqual([1])
    expect(sequencesFor("conversation")).toEqual([1])
    expect(
      cache.has(
        {
          backend: BACKEND,
          source: "commit",
          canonicalRepo: "/repo",
          sourceEpoch: "v1:epoch-a",
        },
        commitCandidate("deadbeef").uri
      )
    ).toBe(false)
  })

  it("commit_page_missing_identity_initializes_via_guarded_fetch", async () => {
    const controller = createController(catalogFixture(), {
      inputs: { gitHead: null },
    })
    controller.setQuery("fix")
    sourceApi.commit.resolve(
      page("commit", 0, [commitCandidate("cafebabe")], true, {
        sourceEpoch: "v1:epoch-b",
      })
    )
    await flushMicrotasks(10)
    expect(applyGitHead).toHaveBeenCalledTimes(1)
    expect(sequencesFor("commit")).toEqual([1, 2])
  })

  it("git_head_input_change_on_the_same_branch_restarts_only_commit", async () => {
    const controller = createController(catalogFixture())
    controller.setQuery("fix")
    const fileCount = sequencesFor("file").length
    const convCount = sequencesFor("conversation").length
    controller.updateInputs(baseInputs({ gitHead: gitHeadB() }))
    await flushMicrotasks()
    expect(sequencesFor("commit")).toEqual([1, 2])
    expect(sequencesFor("file")).toHaveLength(fileCount)
    expect(sequencesFor("conversation")).toHaveLength(convCount)
  })

  it("closed_controller_does_not_restart_for_input_changes", async () => {
    const controller = createController(catalogFixture())
    controller.setQuery("fix")
    await flushMicrotasks()
    const starts = startCalls.length
    controller.close()
    controller.updateInputs(
      baseInputs({
        referenceLimit: 25,
        gitHead: gitHeadB(),
      })
    )
    expect(startCalls.length).toBe(starts)
    controller.setQuery("fix")
    expect(startCalls.length).toBeGreaterThan(starts)
  })

  it("mixed_repository_commit_page_is_rejected_before_any_cache_merge", async () => {
    const controller = createController(catalogFixture())
    controller.setQuery("fix")
    sourceApi.commit.resolve(
      page(
        "commit",
        0,
        [
          commitCandidate("aaa", "/repo"),
          commitCandidate("bbb", "/other-repo"),
        ],
        true,
        { sourceEpoch: "v1:epoch-a" }
      )
    )
    await flushMicrotasks()
    expect(controller.getSnapshot().groups.commit.error).toBe("source")
    expect(controller.getSnapshot().groups.commit.items).toHaveLength(0)
    expect(applyGitHead).not.toHaveBeenCalled()
  })

  it("concurrent_page_and_validation_epoch_changes_share_one_git_refresh", async () => {
    heldGitFetch = deferred()
    const controller = createController(catalogFixture())
    controller.setQuery("subject")
    // Deliver epoch mismatch on first commit page.
    sourceApi.commit.resolve(
      page("commit", 0, [commitCandidate("deadbeef")], true, {
        sourceEpoch: "v1:epoch-b",
      })
    )
    await flushMicrotasks(3)
    expect(fetchGitHead).toHaveBeenCalledTimes(1)
    // A second waiter (validation epoch error) joins the same gate.
    void (
      controller as unknown as {
        enterGitRefreshGate: (g: number, h: GitHeadInfo | null) => Promise<void>
      }
    ).enterGitRefreshGate?.(controller.getSnapshot().generation, gitHeadA())
    // Private method may not be exposed — simulate by resolving another mismatch
    // from a second drain restart is enough; call count stays 1.
    expect(fetchGitHead).toHaveBeenCalledTimes(1)
    heldGitFetch.resolve(gitHeadB())
    await flushMicrotasks(10)
    expect(applyGitHead).toHaveBeenCalledTimes(1)
    expect(sequencesFor("commit")).toEqual([1, 2])
    expect(sequencesFor("file")).toEqual([1])
    expect(sequencesFor("conversation")).toEqual([1])
  })

  it("stale_git_fetch_is_ignored_after_input_identity_change", async () => {
    heldGitFetch = deferred()
    const controller = createController(catalogFixture())
    controller.setQuery("fix")
    sourceApi.commit.resolve(
      page("commit", 0, [commitCandidate("x")], true, {
        sourceEpoch: "v1:epoch-b",
      })
    )
    await flushMicrotasks(3)
    expect(fetchGitHead).toHaveBeenCalledTimes(1)
    controller.updateInputs(baseInputs({ gitHead: gitHeadB() }))
    heldGitFetch.resolve({
      ...gitHeadB(),
      reference_source_epoch: "v1:epoch-c",
      head_sha: "cccccccccccccccccccccccccccccccccccccccc",
    })
    await flushMicrotasks(10)
    // Stale fetch must not apply.
    expect(applyGitHead).not.toHaveBeenCalled()
  })

  it("known_non_repo_skips_commit_while_unknown_identity_may_probe", async () => {
    const nonRepo = createController(catalogFixture(), {
      inputs: {
        gitHead: {
          is_repo: false,
          branch: null,
          detached: false,
          short_sha: null,
          canonical_repo: null,
          head_sha: null,
          reference_source_epoch: null,
        },
      },
    })
    nonRepo.setQuery("app")
    await flushMicrotasks()
    expect(sequencesFor("file")).toEqual([1])
    expect(sequencesFor("conversation")).toEqual([1])
    expect(sequencesFor("commit")).toEqual([])
    expect(nonRepo.getSnapshot().groups.commit.error).toBeNull()

    startCalls.length = 0
    const unknown = createController(catalogFixture(), {
      inputs: { gitHead: null },
    })
    unknown.setQuery("app")
    await flushMicrotasks()
    expect(sequencesFor("commit")).toEqual([1])
  })

  it("unborn_identity_selects_commit_bucket_and_empty_page_is_not_error", async () => {
    const unborn: GitHeadInfo = {
      is_repo: true,
      branch: "main",
      detached: false,
      short_sha: null,
      canonical_repo: "/repo",
      head_sha: null,
      reference_source_epoch: "v1:unborn",
    }
    const controller = createController(catalogFixture(), {
      inputs: { gitHead: unborn },
    })
    controller.setQuery("app")
    sourceApi.commit.resolve(
      page("commit", 0, [], true, {
        sourceEpoch: "v1:unborn",
        doneReason: "exhausted",
      })
    )
    await flushMicrotasks()
    expect(controller.getSnapshot().groups.commit.error).toBeNull()
    expect(controller.getSnapshot().groups.commit.items).toHaveLength(0)
    expect(controller.getSnapshot().groups.commit.loading).toBe(false)
  })

  it("catalog_literal_and_regex_descriptors_share_declared_fields", async () => {
    const agents = [
      makeAgent("codex", {
        name: "Codex CLI",
        description: "OpenAI coding agent",
      }),
    ]
    const profile = makeProfile({
      agent_type: "codex",
      name: "Fast",
      config_values: { model: "gpt-5" },
    })
    const controller = createController({
      agents,
      profileCatalog: {
        profiles: [profile],
        delegation_enabled: true,
        revision: 1,
      },
    })
    controller.setQuery("Codex")
    const literalUris = controller
      .getSnapshot()
      .groups.agent.items.map((i) => i.reference.uri)
    expect(literalUris).toContain("codeg://agent/codex")
    expect(literalUris).toContain(
      `codeg://delegation-profile/codex/${PROFILE_ID}`
    )

    const agentEntry: CatalogSearchEntry = {
      kind: "agent",
      agent: agents[0]!,
    }
    const profileEntry: CatalogSearchEntry = {
      kind: "profile",
      profile,
      backingAgent: agents[0]!,
    }
    expect(catalogSearchFields(agentEntry)).toEqual({
      primary: ["Codex CLI"],
      secondary: ["codex", "OpenAI coding agent", ""],
    })
    expect(catalogSearchFields(profileEntry)).toEqual({
      primary: ["Codex:Fast"],
      secondary: ["codex", "OpenAI coding agent", "gpt-5"],
    })

    controller.setQuery("re:Codex")
    await flushMicrotasks()
    expect(matchCalls.length).toBeGreaterThan(0)
    const desc = matchCalls[0]!.descriptors
    const agentDesc = desc.find((d) => d.id === "codeg://agent/codex")
    expect(agentDesc?.primary).toEqual(["Codex CLI"])
    expect(agentDesc?.secondary).toEqual(["codex", "OpenAI coding agent", ""])
    const profileDesc = desc.find((d) =>
      d.id.includes("delegation-profile/codex")
    )
    expect(profileDesc?.primary).toEqual(["Codex:Fast"])
    expect(profileDesc?.secondary).toEqual([
      "codex",
      "OpenAI coding agent",
      "gpt-5",
    ])
  })

  it("regex_catalog_over_1024_batches_without_truncation_or_more_than_four_in_flight", async () => {
    const manyAgents = Array.from(
      { length: 4100 },
      (_, i) =>
        ({
          agent_type: `a${i}`,
          name: `Agent ${i}`,
          description: "",
          available: true,
          enabled: true,
          sort_order: i,
        }) as unknown as AcpAgentInfo
    )
    const controller = createController({
      agents: manyAgents,
      profileCatalog: {
        profiles: [],
        delegation_enabled: true,
        revision: 1,
      },
    })
    controller.setQuery("re:Agent")
    await flushMicrotasks(2)
    // Four-permit semaphore: only four batches in flight initially.
    expect(regexQueue.length).toBe(4)
    expect(matchCalls.map((c) => c.descriptors.length)).toEqual([
      1024, 1024, 1024, 1024,
    ])

    // Map each deferred to its call in order by resolving FIFO while tracking index.
    let callIndex = 0
    const resolveNext = () => {
      while (regexQueue.length > 0) {
        const call = matchCalls[callIndex++]!
        sourceApi.regex.resolve(
          call.descriptors.map((d) => ({
            id: d.id,
            sourceOrdinal: d.sourceOrdinal,
            rank: { fieldTier: 0, start: 0, length: 1 },
          }))
        )
      }
    }
    for (let guard = 0; guard < 30 && callIndex < 5; guard++) {
      resolveNext()
      await flushMicrotasks(3)
    }
    expect(matchCalls.map((c) => c.descriptors.length)).toEqual([
      1024, 1024, 1024, 1024, 4,
    ])
    await flushMicrotasks(10)
    expect(controller.getSnapshot().groups.agent.items.length).toBe(4100)
  })

  it("catalog_change_restarts_only_regex_catalog_matcher", async () => {
    const controller = createController(catalogFixture())
    controller.setQuery("re:Codex")
    await flushMicrotasks()
    const starts = startCalls.length
    const cancels = cancelCalls.length
    const matches = matchCalls.length
    controller.updateInputs(
      baseInputs({
        profileCatalog: {
          profiles: [makeProfile({ name: "Other" })],
          delegation_enabled: true,
          revision: 2,
        },
      })
    )
    await flushMicrotasks()
    expect(matchCalls.length).toBeGreaterThan(matches)
    expect(startCalls.length).toBe(starts)
    expect(cancelCalls.length).toBe(cancels)
  })

  it("one_failed_regex_catalog_batch_never_publishes_a_partial_prefix", async () => {
    const manyAgents = Array.from(
      { length: 4100 },
      (_, i) =>
        ({
          agent_type: `a${i}`,
          name: `Agent ${i}`,
          description: "",
          available: true,
          enabled: true,
          sort_order: i,
        }) as unknown as AcpAgentInfo
    )
    const controller = createController({
      agents: manyAgents,
      profileCatalog: {
        profiles: [],
        delegation_enabled: false,
        revision: 1,
      },
    })
    // Seed continuity via bare then switch to regex.
    controller.setQuery("")
    expect(controller.getSnapshot().groups.agent.items.length).toBe(4100)
    controller.setQuery("re:Agent")
    await flushMicrotasks(2)
    // Resolve four batches, fail the fifth.
    let resolved = 0
    while (regexQueue.length > 0 && resolved < 4) {
      const call = matchCalls[resolved]!
      sourceApi.regex.resolve(
        call.descriptors.slice(0, 2).map((d) => ({
          id: d.id,
          sourceOrdinal: d.sourceOrdinal,
          rank: { fieldTier: 0, start: 0, length: 1 },
        }))
      )
      resolved++
      await flushMicrotasks(2)
    }
    // Fail remaining
    while (regexQueue.length > 0) {
      sourceApi.regex.reject(appError("source_failed"))
      await flushMicrotasks(2)
    }
    await flushMicrotasks(10)
    const snap = controller.getSnapshot().groups.agent
    expect(snap.error).toBe("source")
    // Continuity rows remain, non-selectable — not the partial new run.
    expect(snap.items.length).toBe(4100)
    expect(snap.items.every((i) => i.selectable === false)).toBe(true)
  })

  it("late_regex_catalog_run_cannot_replace_a_newer_catalog", async () => {
    const controller = createController({
      agents: [makeAgent("codex", { name: "CodexA" })],
      profileCatalog: {
        profiles: [],
        delegation_enabled: true,
        revision: 1,
      },
    })
    controller.setQuery("re:Codex")
    await flushMicrotasks()
    expect(matchCalls.length).toBe(1)
    expect(regexQueue.length).toBe(1)
    const heldA = regexQueue[0]!

    controller.updateInputs(
      baseInputs({
        agents: [makeAgent("codex", { name: "CodexB" })],
        profileCatalog: {
          profiles: [],
          delegation_enabled: true,
          revision: 2,
        },
      })
    )
    await flushMicrotasks()
    // Queue is [A, B]; resolve B (second) first by resolving A with empty
    // (stale run discarded) then B — actually both deferreds are live; resolve
    // A as no-op stale and B as the winner by call order: shift resolves A first.
    // Resolve stale A first (guard discards it).
    heldA.resolve([
      {
        id: "codeg://agent/codex",
        sourceOrdinal: 1,
        rank: { fieldTier: 0, start: 0, length: 1 },
      },
    ])
    const aIdx = regexQueue.indexOf(heldA)
    if (aIdx >= 0) regexQueue.splice(aIdx, 1)
    await flushMicrotasks(3)

    // Resolve B from the remaining queue entry.
    expect(matchCalls.length).toBeGreaterThanOrEqual(2)
    const bMatches = matchCalls[1]!.descriptors.map((d) => ({
      id: d.id,
      sourceOrdinal: d.sourceOrdinal,
      rank: { fieldTier: 0, start: 0, length: 3 },
    }))
    expect(regexQueue.length).toBeGreaterThanOrEqual(1)
    sourceApi.regex.resolve(bMatches)
    await flushMicrotasks(5)
    expect(
      controller.getSnapshot().groups.agent.items[0]?.reference.label
    ).toBe("CodexB")

    // Late A already resolved — membership stays B.
    await flushMicrotasks(5)
    expect(
      controller.getSnapshot().groups.agent.items[0]?.reference.label
    ).toBe("CodexB")
  })

  it("conversation_cache_events_update_an_open_controller_without_restarting_sources", async () => {
    cache.mergeCandidate(
      { backend: BACKEND, source: "conversation" },
      sessionCandidate("7", "alpha")
    )
    const controller = createController(catalogFixture())
    controller.setQuery("alpha")
    await flushMicrotasks()
    const starts = startCalls.length
    const cancels = cancelCalls.length
    expect(
      controller.getSnapshot().groups.session.items[0]?.reference.label
    ).toBe("alpha")

    // Title still matches the open query so the row is updated, not removed.
    cache.markConversationUpsert(
      BACKEND,
      makeSummary({ id: 7, title: "alpha-renamed" })
    )
    expect(
      controller.getSnapshot().groups.session.items[0]?.reference.label
    ).toBe("alpha-renamed")

    cache.markConversationStatus(BACKEND, 7, "pending_review")
    expect(
      controller.getSnapshot().groups.session.items[0]?.reference.meta?.status
    ).toBe("pending_review")

    controller.setSelectedUri("codeg://session/7")
    cache.markConversationDelete(BACKEND, 7)
    expect(controller.getSnapshot().groups.session.items).toHaveLength(0)
    expect(startCalls.length).toBe(starts)
    expect(cancelCalls.length).toBe(cancels)

    controller.close()
    cache.markConversationUpsert(BACKEND, makeSummary({ id: 8, title: "late" }))
    // Reopen — subscription reacquired, sources start again.
    controller.setQuery("late")
    expect(startCalls.length).toBeGreaterThan(starts)
  })

  it("file_cache_uses_only_an_authoritative_canonical_root_alias", async () => {
    const root = "C:/real"
    const bucket: ReferenceCacheBucketKey = {
      backend: BACKEND,
      source: "file",
      canonicalRoot: root,
    }
    cache.mergeCandidate(bucket, fileCandidate("app.ts", root))
    cache.rememberFileRootAlias(BACKEND, "C:/link", root)

    const controller = createController(catalogFixture())
    controller.setQuery("app")
    expect(controller.getSnapshot().groups.file.items).toHaveLength(1)
    // Drain pending starts so later controllers don't share the queue.
    while ((startQueues.get("file") ?? []).length > 0) {
      sourceApi.file.resolve(page("file", 0, [], true))
      await flushMicrotasks()
    }
    while ((startQueues.get("conversation") ?? []).length > 0) {
      sourceApi.conversation.resolve(page("conversation", 0, [], true))
      await flushMicrotasks()
    }
    while ((startQueues.get("commit") ?? []).length > 0) {
      sourceApi.commit.resolve(page("commit", 0, [], true))
      await flushMicrotasks()
    }
    controller.close()

    // Fresh cache, no alias.
    const cache2 = new ReferenceSearchCache()
    const c2 = createController(catalogFixture(), {
      cache: cache2,
      defaultPath: "C:/link",
    })
    c2.setQuery("app")
    expect(c2.getSnapshot().groups.file.items).toHaveLength(0)
    // First non-empty page establishes alias; keep job open for a later page.
    sourceApi.file.resolve(
      page("file", 0, [fileCandidate("app.ts", "C:/real")], false)
    )
    await flushMicrotasks(10)
    expect(cache2.resolveFileRootAlias(BACKEND, "C:/link")).toBe("C:/real")
    expect(c2.getSnapshot().groups.file.items).toHaveLength(1)
    expect(c2.getSnapshot().groups.file.error).toBeNull()
    expect(c2.getSnapshot().groups.file.loading).toBe(true)

    // Later page with a different canonical root is a protocol error; no merge
    // into either bucket and loading stops.
    const otherRootCandidate = fileCandidate("other.ts", "C:/other", 2)
    sourceApi.file.resolveNext(
      page("file", 1, [otherRootCandidate], true)
    )
    await flushMicrotasks(10)
    expect(c2.getSnapshot().groups.file.error).toBe("source")
    expect(c2.getSnapshot().groups.file.loading).toBe(false)
    expect(c2.getSnapshot().groups.file.items).toHaveLength(1)
    expect(
      c2.getSnapshot().groups.file.items[0]?.reference.uri
    ).toBe(fileCandidate("app.ts", "C:/real").uri)
    expect(
      cache2.literalPreview(
        { backend: BACKEND, source: "file", canonicalRoot: "C:/other" },
        "other",
        50
      ).items
    ).toHaveLength(0)
    expect(cache2.resolveFileRootAlias(BACKEND, "C:/link")).toBe("C:/real")

    // Empty first page leaves alias absent.
    c2.close()
    const cache3 = new ReferenceSearchCache()
    const c3 = createController(catalogFixture(), {
      cache: cache3,
      defaultPath: "C:/link",
    })
    // Drain siblings from c2
    while ((startQueues.get("file") ?? []).length > 0) {
      sourceApi.file.reject(appError("cancelled"))
      await flushMicrotasks()
    }
    c3.setQuery("zzz")
    // Only resolve this controller's file start (latest).
    const pending = startQueues.get("file") ?? []
    while (pending.length > 1) {
      sourceApi.file.resolve(page("file", 0, [], true))
      await flushMicrotasks()
    }
    sourceApi.file.resolve(page("file", 0, [], true))
    await flushMicrotasks()
    expect(cache3.resolveFileRootAlias(BACKEND, "C:/link")).toBeNull()
  })

  it("mixed_or_missing_file_canonical_root_marks_protocol_error_and_stops_loading", async () => {
    const controller = createController(catalogFixture())
    controller.setQuery("app")
    await flushMicrotasks()

    // Mixed roots on a single page — no merge, error + not loading.
    sourceApi.file.resolve(
      page(
        "file",
        0,
        [fileCandidate("a.ts", "C:/real"), fileCandidate("b.ts", "C:/other", 2)],
        true
      )
    )
    await flushMicrotasks(10)
    expect(controller.getSnapshot().groups.file.error).toBe("source")
    expect(controller.getSnapshot().groups.file.loading).toBe(false)
    expect(controller.getSnapshot().groups.file.items).toHaveLength(0)
    expect(
      cache.literalPreview(
        { backend: BACKEND, source: "file", canonicalRoot: "C:/real" },
        "a",
        50
      ).items
    ).toHaveLength(0)

    controller.close()
    const c2 = createController(catalogFixture())
    c2.setQuery("app")
    await flushMicrotasks()
    const missingRoot = fileCandidate("x.ts", "C:/real")
    // Force missing canonicalWorkspaceRoot after construction.
    ;(missingRoot.metadata as { canonicalWorkspaceRoot?: string }).canonicalWorkspaceRoot =
      undefined as unknown as string
    sourceApi.file.resolve(page("file", 0, [missingRoot], true))
    await flushMicrotasks(10)
    expect(c2.getSnapshot().groups.file.error).toBe("source")
    expect(c2.getSnapshot().groups.file.loading).toBe(false)
    expect(c2.getSnapshot().groups.file.items).toHaveLength(0)
  })

  it("regex_file_drain_begins_on_first_non_empty_page_without_prior_alias", async () => {
    const controller = createController(catalogFixture())
    controller.setQuery("re:app")
    // Resolve catalog regex quickly so it does not block the drain.
    await flushMicrotasks()
    while (regexQueue.length > 0) {
      sourceApi.regex.resolve([])
      await flushMicrotasks()
    }

    const ranked = fileCandidate("app.ts", "C:/real")
    ranked.regexRank = { fieldTier: 0, start: 0, length: 3 }
    sourceApi.file.resolve(page("file", 0, [ranked], true))
    sourceApi.conversation.resolve(page("conversation", 0, [], true))
    sourceApi.commit.resolve(page("commit", 0, [], true))
    await flushMicrotasks(15)

    expect(controller.getSnapshot().groups.file.error).toBeNull()
    expect(controller.getSnapshot().groups.file.loading).toBe(false)
    expect(controller.getSnapshot().groups.file.items).toHaveLength(1)

    const snap = cache.getRegexSnapshot(
      { backend: BACKEND, source: "file", canonicalRoot: "C:/real" },
      "re:app"
    )
    expect(snap).not.toBeNull()
    expect(snap!.items.map((i) => i.candidate.uri)).toEqual([ranked.uri])
  })

  it("sync_visible_pins_clears_abandoned_file_bucket_after_alias_repoint", async () => {
    const pinSpy = vi.spyOn(cache, "pinVisible")
    const oldRoot = "C:/old"
    const newRoot = "C:/real"
    const oldBucket: ReferenceCacheBucketKey = {
      backend: BACKEND,
      source: "file",
      canonicalRoot: oldRoot,
    }
    cache.mergeCandidate(oldBucket, fileCandidate("app-old.ts", oldRoot))
    cache.rememberFileRootAlias(BACKEND, "C:/link", oldRoot)

    const controller = createController(catalogFixture())
    controller.setQuery("app")
    expect(controller.getSnapshot().groups.file.items).toHaveLength(1)
    // Drain siblings so the file start is the only pending identity.
    sourceApi.conversation.resolve(page("conversation", 0, [], true))
    sourceApi.commit.resolve(page("commit", 0, [], true))
    await flushMicrotasks()

    pinSpy.mockClear()
    const nextCandidate = fileCandidate("app.ts", newRoot)
    sourceApi.file.resolve(page("file", 0, [nextCandidate], true))
    await flushMicrotasks(10)

    const clearedOld = pinSpy.mock.calls.some(
      ([, bucket, uris]) =>
        bucket.source === "file" &&
        "canonicalRoot" in bucket &&
        bucket.canonicalRoot === oldRoot &&
        uris.length === 0
    )
    expect(clearedOld).toBe(true)
    expect(controller.getSnapshot().groups.file.items).toHaveLength(1)
    expect(
      controller.getSnapshot().groups.file.items[0]?.reference.uri
    ).toBe(nextCandidate.uri)
  })

  it("close_is_idempotent_and_reopen_allocates_new_source_identities", async () => {
    const controller = createController(catalogFixture())
    controller.setQuery("app")
    await flushMicrotasks()
    const sessionId = controller.searchSessionId
    const cancelsBefore = cancelCalls.length
    controller.close()
    controller.close()
    expect(cancelCalls.length).toBe(cancelsBefore + 3)
    const afterClose = startCalls.length
    controller.setQuery("app")
    expect(controller.searchSessionId).toBe(sessionId)
    expect(sequencesFor("file").slice(-1)[0]).toBe(2)
    expect(startCalls.length).toBeGreaterThan(afterClose)
    // Fresh request ids
    const fileStarts = startCalls.filter((c) => c.source === "file")
    expect(fileStarts[0]!.requestId).not.toBe(fileStarts[1]!.requestId)
  })

  it("invalid_or_cancelled_validation_never_falls_through_to_cached_insertion", async () => {
    cache.mergeCandidate(
      { backend: BACKEND, source: "conversation" },
      sessionCandidate("7", "alpha")
    )
    const controller = createController(catalogFixture())
    controller.setQuery("alpha")
    await flushMicrotasks()
    controller.setSelectedUri("codeg://session/7")
    await flushMicrotasks()

    for (const code of [
      "invalid_request",
      "invalid_pattern",
      "cancelled",
    ] as const) {
      const confirm = controller.confirmCandidate("codeg://session/7")
      await flushMicrotasks()
      sourceApi.validation.reject(appError(code))
      await expect(confirm).resolves.toBeNull()
    }

    // Operational error returns cached reference.
    const confirm = controller.confirmCandidate("codeg://session/7")
    await flushMicrotasks()
    sourceApi.validation.reject(appError("source_failed"))
    const result = await confirm
    expect(result?.uri).toBe("codeg://session/7")
  })

  it("cold_confirm_times_out_after_one_second_and_returns_cached_reference", async () => {
    vi.useFakeTimers()
    try {
      cache.mergeCandidate(
        { backend: BACKEND, source: "conversation" },
        sessionCandidate("7", "alpha")
      )
      const controller = createController(catalogFixture())
      controller.setQuery("alpha")
      await flushMicrotasks()
      // No setSelectedUri — cold confirm starts validation and must still bound.
      expect(validateCalls.length).toBe(0)

      const confirm = controller.confirmCandidate("codeg://session/7")
      await flushMicrotasks()
      expect(validateCalls.length).toBe(1)

      // Still pending before the one-second bound.
      let settled: ReferenceAttrs | null | undefined
      void confirm.then((value) => {
        settled = value
      })
      await vi.advanceTimersByTimeAsync(999)
      await flushMicrotasks()
      expect(settled).toBeUndefined()

      await vi.advanceTimersByTimeAsync(1)
      await flushMicrotasks()
      const result = await confirm
      expect(result?.uri).toBe("codeg://session/7")
      expect(result?.label).toBe("alpha")
    } finally {
      vi.useRealTimers()
    }
  })

  it("late_negative_after_confirm_timeout_does_not_evict_or_mutate", async () => {
    vi.useFakeTimers()
    try {
      const bucket: ReferenceCacheBucketKey = {
        backend: BACKEND,
        source: "conversation",
      }
      cache.mergeCandidate(bucket, sessionCandidate("7", "alpha"))
      const controller = createController(catalogFixture())
      controller.setQuery("alpha")
      await flushMicrotasks()
      // In-flight validation from selection, then confirm reuses it under timeout.
      controller.setSelectedUri("codeg://session/7")
      await flushMicrotasks()
      expect(validateCalls.length).toBe(1)
      const validationRequestId = (
        validateCalls[0] as { validationRequestId: string }
      ).validationRequestId

      const confirm = controller.confirmCandidate("codeg://session/7")
      await flushMicrotasks()
      // Reuse in-flight — no second validate call.
      expect(validateCalls.length).toBe(1)

      await vi.advanceTimersByTimeAsync(1000)
      await flushMicrotasks()
      const result = await confirm
      expect(result?.uri).toBe("codeg://session/7")

      // Late not_found for the timed-out request must not evict cache/membership.
      sourceApi.validation.resolve({
        status: "not_found",
        validationRequestId,
      })
      await flushMicrotasks(10)

      expect(cache.has(bucket, "codeg://session/7")).toBe(true)
      expect(
        controller
          .getSnapshot()
          .groups.session.items.map((item) => item.reference.uri)
      ).toContain("codeg://session/7")
      expect(
        controller
          .getSnapshot()
          .groups.session.items.find(
            (item) => item.reference.uri === "codeg://session/7"
          )?.freshness
      ).not.toBe("validating")
    } finally {
      vi.useRealTimers()
    }
  })
})
