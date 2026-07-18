import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

import type {
  DbConversationSummary,
  ReferenceCandidate,
  ReferenceRegexRank,
} from "@/lib/types"
import {
  ReferenceSearchCache,
  candidateSearchFields,
  rankLiteralFields,
  referenceSearchCache,
  type ReferenceCacheBucketKey,
} from "@/lib/reference-search-cache"

function fileBucket(
  backend: string,
  canonicalRoot: string
): ReferenceCacheBucketKey {
  return { backend, source: "file", canonicalRoot }
}

function conversationBucket(backend: string): ReferenceCacheBucketKey {
  return { backend, source: "conversation" }
}

function commitBucket(
  backend: string,
  canonicalRepo: string,
  sourceEpoch: string
): ReferenceCacheBucketKey {
  return { backend, source: "commit", canonicalRepo, sourceEpoch }
}

function candidate(uri: string, label: string): ReferenceCandidate {
  const relativePath = label.includes("/") ? label : `src/${label}`
  return {
    source: "file",
    uri,
    id: uri,
    label,
    detail: relativePath,
    keywords: `${label} ${relativePath}`,
    metadata: {
      kind: "file",
      canonicalWorkspaceRoot: "C:/repo",
      relativePath,
      entryKind: "file",
    },
    sourceOrdinal: 1,
    regexRank: null,
  }
}

function sessionCandidate(id: string, label?: string): ReferenceCandidate {
  const n = Number(id)
  const resolvedLabel = label ?? `#${id}`
  return {
    source: "conversation",
    uri: `codeg://session/${id}`,
    id,
    label: resolvedLabel,
    detail: "in_progress",
    keywords: `${resolvedLabel} claude_code`,
    metadata: {
      kind: "conversation",
      conversationId: n,
      agentType: "claude_code",
      status: "in_progress",
      branch: null,
      projectName: "codeg",
      projectPath: "/repo/codeg",
    },
    sourceOrdinal: n,
    regexRank: null,
  }
}

function regexRank(
  fieldTier: number,
  start: number,
  length: number
): ReferenceRegexRank {
  return { fieldTier, start, length }
}

function makeSummary(
  overrides: Partial<DbConversationSummary> & { id: number }
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
    ...overrides,
  }
}

describe("candidateSearchFields / rankLiteralFields", () => {
  it("maps declared primary/secondary fields by source", () => {
    const file = candidate("file:///C%3A/repo/src/app.ts", "app.ts")
    expect(candidateSearchFields(file)).toEqual({
      primary: ["app.ts"],
      secondary: ["src/app.ts"],
    })

    const session = sessionCandidate("7", "Fix login")
    expect(candidateSearchFields(session)).toEqual({
      primary: ["Fix login"],
      secondary: [
        "7",
        "claude_code",
        "in_progress",
        "",
        "codeg",
        "/repo/codeg",
      ],
    })

    const commit: ReferenceCandidate = {
      source: "commit",
      uri: "codeg://commit/%2Frepo@abc",
      id: "abc",
      label: "abc1234",
      detail: "subject",
      keywords: "abc subject",
      metadata: {
        kind: "commit",
        canonicalRepo: "/repo",
        fullHash: "abcdef0123456789",
        shortHash: "abcdef0",
        subject: "fix bug",
        message: "fix bug\n\nbody",
        author: "dev",
        authoredAt: "2026-01-01T00:00:00Z",
      },
      sourceOrdinal: 1,
      regexRank: null,
    }
    expect(candidateSearchFields(commit)).toEqual({
      primary: ["abcdef0", "abcdef0123456789", "fix bug"],
      secondary: ["fix bug\n\nbody", "dev"],
    })
  })

  it("ranks literal tiers 0-4 with declared-order ties", () => {
    expect(rankLiteralFields("app", ["app"], [])).toBe(0)
    expect(rankLiteralFields("ap", ["app"], [])).toBe(1)
    expect(rankLiteralFields("file", ["my-file.ts"], [])).toBe(2)
    expect(rankLiteralFields("pp", ["app"], [])).toBe(3)
    expect(rankLiteralFields("codeg", ["title"], ["7", "codeg"])).toBe(4)
    expect(rankLiteralFields("zzz", ["app"], ["src"])).toBeNull()
  })
})

describe("ReferenceSearchCache", () => {
  afterEach(() => {
    referenceSearchCache.reset()
  })

  it("reuses literal items and exact regex snapshots without duplicating metadata", () => {
    const cache = new ReferenceSearchCache()
    const bucket = fileBucket("local:tauri", "C:/repo")
    cache.mergeCandidate(
      bucket,
      candidate("file:///C%3A/repo/src/app.ts", "app.ts")
    )
    const refresh = cache.beginRegexRefresh("controller", bucket, "re:^src/")
    cache.commitRegexRefresh(
      refresh,
      [{ uri: "file:///C%3A/repo/src/app.ts", rank: regexRank(1, 0, 3) }],
      false
    )
    expect(
      cache
        .literalPreview(bucket, "APP", 50)
        .items.map((item) => item.candidate.uri)
    ).toEqual(["file:///C%3A/repo/src/app.ts"])
    expect(
      cache.getRegexSnapshot(bucket, "re:^src/")?.items[0].candidate.label
    ).toBe("app.ts")
    expect(cache.debugStats().candidateCount).toBe(1)
  })

  it("preserves truncated on literal limit and regex commit", () => {
    const cache = new ReferenceSearchCache()
    const bucket = fileBucket("local:tauri", "C:/repo")
    for (const name of ["a.ts", "b.ts", "c.ts"]) {
      cache.mergeCandidate(
        bucket,
        candidate(`file:///C%3A/repo/src/${name}`, name)
      )
    }
    const preview = cache.literalPreview(bucket, "ts", 2)
    expect(preview.items).toHaveLength(2)
    expect(preview.truncated).toBe(true)

    const refresh = cache.beginRegexRefresh("c", bucket, "re:ts")
    cache.commitRegexRefresh(
      refresh,
      [
        {
          uri: "file:///C%3A/repo/src/a.ts",
          rank: regexRank(0, 2, 2),
        },
      ],
      true
    )
    expect(cache.getRegexSnapshot(bucket, "re:ts")?.truncated).toBe(true)
  })

  it("pins selected and visible entries while enforcing global LRUs", () => {
    const cache = new ReferenceSearchCache({
      candidateCap: 3,
      regexSnapshotCap: 2,
    })
    const bucket = conversationBucket("local:tauri")
    for (const id of ["1", "2", "3"])
      cache.mergeCandidate(bucket, sessionCandidate(id))
    cache.pinSelected("controller", bucket, "codeg://session/1")
    cache.pinVisible("controller", bucket, ["codeg://session/2"])
    cache.mergeCandidate(bucket, sessionCandidate("4"))
    expect(cache.has(bucket, "codeg://session/1")).toBe(true)
    expect(cache.has(bucket, "codeg://session/2")).toBe(true)
    expect(cache.has(bucket, "codeg://session/3")).toBe(false)
  })

  it("visible_pins_coexist_across_buckets_and_replace_per_bucket", () => {
    const cache = new ReferenceSearchCache({
      candidateCap: 2,
      regexSnapshotCap: 2,
    })
    const file = fileBucket("local:tauri", "C:/repo")
    const conv = conversationBucket("local:tauri")
    const commit = commitBucket("local:tauri", "/repo", "epoch-1")
    cache.mergeCandidate(file, candidate("file:///C%3A/repo/a.ts", "a.ts"))
    cache.mergeCandidate(conv, sessionCandidate("1"))
    cache.pinVisible("controller", file, ["file:///C%3A/repo/a.ts"])
    cache.pinVisible("controller", conv, ["codeg://session/1"])
    cache.mergeCandidate(file, candidate("file:///C%3A/repo/b.ts", "b.ts"))
    cache.mergeCandidate(file, candidate("file:///C%3A/repo/c.ts", "c.ts"))
    expect(cache.has(file, "file:///C%3A/repo/a.ts")).toBe(true)
    expect(cache.has(conv, "codeg://session/1")).toBe(true)

    cache.pinVisible("controller", file, [])
    cache.mergeCandidate(file, candidate("file:///C%3A/repo/d.ts", "d.ts"))
    cache.mergeCandidate(file, candidate("file:///C%3A/repo/e.ts", "e.ts"))
    expect(cache.has(file, "file:///C%3A/repo/a.ts")).toBe(false)
    expect(cache.has(conv, "codeg://session/1")).toBe(true)

    const commitUri = "codeg://commit/%2Frepo@abc"
    cache.mergeCandidate(commit, {
      source: "commit",
      uri: commitUri,
      id: "abc",
      label: "abc",
      detail: null,
      keywords: "abc",
      metadata: {
        kind: "commit",
        canonicalRepo: "/repo",
        fullHash: "abcdef0",
        shortHash: "abc",
        subject: "s",
        message: "m",
        author: "a",
        authoredAt: "2026-01-01T00:00:00Z",
      },
      sourceOrdinal: 1,
      regexRank: null,
    })
    cache.pinSelected("controller", commit, commitUri)
    expect(cache.has(commit, commitUri)).toBe(true)
    cache.pinSelected("controller", file, "file:///C%3A/repo/d.ts")
    cache.mergeCandidate(commit, {
      source: "commit",
      uri: "codeg://commit/%2Frepo@def",
      id: "def",
      label: "def",
      detail: null,
      keywords: "def",
      metadata: {
        kind: "commit",
        canonicalRepo: "/repo",
        fullHash: "def456",
        shortHash: "def",
        subject: "s2",
        message: "m2",
        author: "a",
        authoredAt: "2026-01-01T00:00:00Z",
      },
      sourceOrdinal: 2,
      regexRank: null,
    })
    // Prior selected pin released; conversation visible pin still holds.
    expect(cache.has(commit, commitUri)).toBe(false)
    expect(cache.has(conv, "codeg://session/1")).toBe(true)
  })

  it("evicts on UTF-8 byte pressure and prunes pinned overflow after release", () => {
    const bucket = fileBucket("local:tauri", "C:/repo")
    const small = candidate("file:///C%3A/repo/a.ts", "a.ts")
    const large = candidate("file:///C%3A/repo/large.ts", "\u754c".repeat(4096))
    const grown = candidate(small.uri, "\u754c".repeat(4096))
    const probe = new ReferenceSearchCache({
      candidateCap: 10,
      candidateByteCap: Number.MAX_SAFE_INTEGER,
      regexSnapshotCap: 2,
    })
    probe.mergeCandidate(bucket, small)
    const smallBytes = probe.debugStats().candidateBytes
    probe.reset()
    probe.mergeCandidate(bucket, large)
    const largeBytes = probe.debugStats().candidateBytes
    probe.reset()
    probe.mergeCandidate(bucket, grown)
    const grownBytes = probe.debugStats().candidateBytes

    const pressured = new ReferenceSearchCache({
      candidateCap: 10,
      candidateByteCap: largeBytes,
      regexSnapshotCap: 2,
    })
    pressured.mergeCandidate(bucket, small)
    pressured.mergeCandidate(bucket, large)
    expect(pressured.has(bucket, small.uri)).toBe(false)
    expect(pressured.has(bucket, large.uri)).toBe(true)
    expect(pressured.debugStats().candidateBytes).toBe(largeBytes)

    const pinned = new ReferenceSearchCache({
      candidateCap: 10,
      candidateByteCap: smallBytes,
      regexSnapshotCap: 2,
    })
    pinned.mergeCandidate(bucket, small)
    pinned.pinSelected("controller", bucket, small.uri)
    pinned.mergeCandidate(bucket, grown)
    expect(grownBytes).toBeGreaterThan(smallBytes)
    expect(pinned.has(bucket, small.uri)).toBe(true)
    expect(pinned.debugStats().candidateBytes).toBe(grownBytes)
    pinned.releaseController("controller")
    expect(pinned.has(bucket, small.uri)).toBe(false)
    expect(pinned.debugStats().candidateBytes).toBe(0)
  })

  it("rejects an old not_found after a fresh page mutates the same URI", () => {
    const cache = new ReferenceSearchCache()
    const bucket = conversationBucket("local:tauri")
    const old = cache.mergeCandidate(bucket, sessionCandidate("7"))!
    const fresh = cache.mergeCandidate(bucket, sessionCandidate("7"))!
    expect(fresh.mutationRevision).toBeGreaterThan(old.mutationRevision)
    expect(
      cache.evictIfRevision(bucket, fresh.candidate.uri, old.mutationRevision)
    ).toBe(false)
    expect(
      cache.markConversationNotFoundIfRevision(
        "local:tauri",
        fresh.candidate.uri,
        old.mutationRevision
      )
    ).toBe(false)
    expect(cache.has(bucket, "codeg://session/7")).toBe(true)

    expect(
      cache.markConversationNotFoundIfRevision(
        "local:tauri",
        fresh.candidate.uri,
        fresh.mutationRevision
      )
    ).toBe(true)
    expect(cache.has(bucket, "codeg://session/7")).toBe(false)
    expect(cache.mergeCandidate(bucket, sessionCandidate("7"))).toBeNull()
  })

  it("resolves only authoritative file aliases and never rewinds mutation revisions", () => {
    const cache = new ReferenceSearchCache()
    expect(cache.resolveFileRootAlias("local:tauri", "C:/repo-link")).toBeNull()
    cache.rememberFileRootAlias("local:tauri", "C:/repo-link", "C:/repo")
    expect(cache.resolveFileRootAlias("local:tauri", "C:/repo-link")).toBe(
      "C:/repo"
    )
    const before = cache.mergeCandidate(
      fileBucket("local:tauri", "C:/repo"),
      candidate("file:///C%3A/repo/a.ts", "a.ts")
    )!
    cache.reset()
    expect(cache.resolveFileRootAlias("local:tauri", "C:/repo-link")).toBeNull()
    cache.rememberFileRootAlias("local:tauri", "C:/repo-link", "C:/repo")
    const after = cache.mergeCandidate(
      fileBucket("local:tauri", "C:/repo"),
      candidate("file:///C%3A/repo/a.ts", "a.ts")
    )!
    expect(after.mutationRevision).toBeGreaterThan(before.mutationRevision)
  })

  it("conversation_upsert_refolds_title_and_preserves_joined_project_metadata", () => {
    const cache = new ReferenceSearchCache()
    const bucket = conversationBucket("local:tauri")
    cache.mergeCandidate(bucket, {
      ...sessionCandidate("3", "old"),
      metadata: {
        kind: "conversation",
        conversationId: 3,
        agentType: "claude_code",
        status: "in_progress",
        branch: "feature",
        projectName: "JoinedProj",
        projectPath: "/join/path",
      },
    })
    cache.markConversationUpsert(
      "local:tauri",
      makeSummary({
        id: 3,
        title: "[README](file:///repo/README.md)",
        agent_type: "codex",
        status: "pending_review",
        git_branch: "main",
      })
    )
    const items = cache.literalPreview(bucket, "README", 10).items
    expect(items).toHaveLength(1)
    const c = items[0]!.candidate
    expect(c.label).toBe("README")
    expect(c.metadata).toMatchObject({
      kind: "conversation",
      agentType: "codex",
      status: "pending_review",
      branch: "main",
      projectName: "JoinedProj",
      projectPath: "/join/path",
    })
  })

  it("conversation_changes_publish_once_after_cache_mutation", () => {
    const cache = new ReferenceSearchCache()
    const bucket = conversationBucket("local:tauri")
    cache.mergeCandidate(bucket, sessionCandidate("1"))
    const events: unknown[] = []
    const unsub = cache.subscribeConversationChanges((e) => {
      events.push(e)
    })

    cache.markConversationUpsert(
      "local:tauri",
      makeSummary({ id: 1, title: "Cached" })
    )
    cache.markConversationUpsert(
      "local:tauri",
      makeSummary({ id: 99, title: "Uncached" })
    )
    cache.markConversationStatus("local:tauri", 1, "pending_review")
    cache.markConversationStatus("local:tauri", 99, "cancelled")
    cache.markConversationDelete("local:tauri", 1)

    expect(events).toEqual([
      {
        kind: "upsert",
        backend: "local:tauri",
        uri: "codeg://session/1",
        summary: expect.objectContaining({ id: 1, title: "Cached" }),
        mutationRevision: expect.any(Number),
      },
      {
        kind: "upsert",
        backend: "local:tauri",
        uri: "codeg://session/99",
        summary: expect.objectContaining({ id: 99 }),
        mutationRevision: null,
      },
      {
        kind: "status",
        backend: "local:tauri",
        uri: "codeg://session/1",
        status: "pending_review",
        mutationRevision: expect.any(Number),
      },
      {
        kind: "status",
        backend: "local:tauri",
        uri: "codeg://session/99",
        status: "cancelled",
        mutationRevision: null,
      },
      {
        kind: "delete",
        backend: "local:tauri",
        uri: "codeg://session/1",
      },
    ])
    expect(
      (events[0] as { mutationRevision: number | null }).mutationRevision
    ).not.toBeNull()
    expect(
      (events[2] as { mutationRevision: number | null }).mutationRevision
    ).not.toBeNull()

    const before = events.length
    unsub()
    cache.markConversationDelete("local:tauri", 2)
    expect(events).toHaveLength(before)
  })

  it("conversation_delete_tombstone_rejects_late_page_and_upsert", () => {
    const cache = new ReferenceSearchCache()
    const bucket = conversationBucket("local:tauri")
    cache.mergeCandidate(bucket, sessionCandidate("7"))
    cache.markConversationDelete("local:tauri", 7)
    expect(cache.mergeCandidate(bucket, sessionCandidate("7"))).toBeNull()
    cache.markConversationUpsert("local:tauri", makeSummary({ id: 7 }))
    cache.markConversationStatus("local:tauri", 7, "cancelled")
    expect(cache.has(bucket, "codeg://session/7")).toBe(false)

    // 513 additional distinct tombstones → FIFO drops the oldest (uri 7).
    for (let i = 0; i < 513; i++) {
      cache.markConversationDelete("local:tauri", 1000 + i)
    }
    // Newest remains tombstoned; oldest (7) was evicted from the 512-cap set.
    expect(cache.mergeCandidate(bucket, sessionCandidate("1512"))).toBeNull()
    expect(cache.mergeCandidate(bucket, sessionCandidate("7"))).not.toBeNull()
    // Capacity stays at 512: a mid-range tombstone still blocks.
    expect(cache.mergeCandidate(bucket, sessionCandidate("1200"))).toBeNull()
  })

  it("conversation_event_watermark_rejects_an_older_page_without_adding_unrelated_rows", () => {
    const cache = new ReferenceSearchCache()
    const bucket = conversationBucket("local:tauri")
    const started = cache.captureMutationRevision()
    cache.markConversationUpsert(
      "local:tauri",
      makeSummary({ id: 9, title: "Live" })
    )
    expect(
      cache.mergeCandidate(bucket, sessionCandidate("9", "page-old"), {
        conversationPageStartedAt: started,
      })
    ).toBeNull()
    expect(cache.has(bucket, "codeg://session/9")).toBe(false)

    const afterEvent = cache.captureMutationRevision()
    const merged = cache.mergeCandidate(
      bucket,
      sessionCandidate("9", "page-new"),
      {
        conversationPageStartedAt: afterEvent,
      }
    )
    expect(merged).not.toBeNull()
    expect(cache.has(bucket, "codeg://session/9")).toBe(true)
  })

  it("conversation_change_during_regex_refresh_cannot_commit_an_old_rank_as_fresh", () => {
    const cache = new ReferenceSearchCache()
    const bucket = conversationBucket("local:tauri")
    cache.mergeCandidate(bucket, sessionCandidate("5", "Alpha"))
    const refresh = cache.beginRegexRefresh("ctl", bucket, "re:Alpha")
    cache.markConversationUpsert(
      "local:tauri",
      makeSummary({ id: 5, title: "Beta" })
    )
    cache.commitRegexRefresh(
      refresh,
      [{ uri: "codeg://session/5", rank: regexRank(0, 0, 5) }],
      false
    )
    const snap = cache.getRegexSnapshot(bucket, "re:Alpha")
    expect(snap?.items ?? []).toHaveLength(0)
    // Candidate remains resolvable for selected pending-validation.
    expect(cache.has(bucket, "codeg://session/5")).toBe(true)
    expect(
      cache.literalPreview(bucket, "Beta", 10).items[0]?.candidate.label
    ).toBe("Beta")
  })

  it("subscribe failures do not prevent later listeners", () => {
    const cache = new ReferenceSearchCache()
    const good = vi.fn()
    cache.subscribeConversationChanges(() => {
      throw new Error("boom")
    })
    cache.subscribeConversationChanges(good)
    cache.markConversationDelete("local:tauri", 1)
    expect(good).toHaveBeenCalledTimes(1)
  })
})

describe("referenceSearchCache singleton reset registration", () => {
  beforeEach(() => {
    referenceSearchCache.reset()
  })

  it("exposes the window singleton", () => {
    expect(referenceSearchCache).toBeInstanceOf(ReferenceSearchCache)
  })
})
