import { describe, expect, it } from "vitest"
import type { DbConversationSummary } from "@/lib/types"
import {
  applyReorder,
  buildOwnerHeaderIndex,
  buildRows,
  computeStickyState,
  flatIndexOfConversation,
  folderHeaderFlatIndices,
  formatRelative,
  groupByFolderWithReuse,
  headerIndexForFolder,
  mergeChildrenById,
  nextHeaderAfter,
  pointerYToTargetIndex,
  reuseSelected,
  reuseSet,
  selectChatConversationsWithReuse,
  selectPinnedWithReuse,
  sidebarRowKey,
  type SidebarBucketKey,
  type SidebarRow,
} from "./sidebar-conversation-grouping"

const MINUTE = 60_000

function conv(
  id: number,
  folderId: number,
  overrides: Partial<DbConversationSummary> = {}
): DbConversationSummary {
  const createdAt = new Date(1_700_000_000_000 + id * MINUTE).toISOString()
  return {
    id,
    folder_id: folderId,
    title: `conv-${id}`,
    title_locked: false,
    agent_type: "claude_code",
    status: "pending",
    awaiting_reply_token: null,
    kind: "regular",
    model: null,
    git_branch: null,
    external_id: null,
    message_count: 0,
    child_count: 0,
    created_at: createdAt,
    updated_at: createdAt,
    pinned_at: null,
    ...overrides,
  }
}

function conversationRow(
  conversation: DbConversationSummary,
  depth: number,
  rootId: number,
  bucketKey: SidebarBucketKey
): SidebarRow {
  return { kind: "conversation", conversation, depth, rootId, bucketKey }
}

describe("formatRelative", () => {
  const now = 1_700_000_000_000

  it("returns an empty string for an unparseable timestamp", () => {
    expect(formatRelative("", now)).toBe("")
    expect(formatRelative("not-a-date", now)).toBe("")
  })

  it.each([
    [30_000, "now"],
    [5 * MINUTE, "5m"],
    [59 * MINUTE, "59m"],
    [60 * MINUTE, "1h"],
    [61 * MINUTE, "1h1m"],
    [(3 * 60 + 5) * MINUTE, "3h5m"],
    [(3 * 60 + 25) * MINUTE, "3h25m"],
    [(9 * 60 + 59) * MINUTE, "9h59m"],
    [10 * 60 * MINUTE, "10h"],
    [(23 * 60 + 59) * MINUTE, "23h"],
    [24 * 60 * MINUTE, "1d"],
    [2 * 24 * 60 * MINUTE, "2d"],
  ])("formats %i milliseconds as %s", (elapsed, label) => {
    expect(formatRelative(new Date(now - elapsed).toISOString(), now)).toBe(
      label
    )
  })

  it("is deterministic for a given `now` regardless of the wall clock", () => {
    const iso = new Date(now - 5 * MINUTE).toISOString()
    // Same inputs → identical string, which is what keeps the card memo hit
    // across re-renders within one minute.
    expect(formatRelative(iso, now)).toBe(formatRelative(iso, now))
  })

  it("ages the label when `now` crosses a unit boundary", () => {
    const iso = new Date(now - 59 * MINUTE).toISOString()
    expect(formatRelative(iso, now)).toBe("59m")
    expect(formatRelative(iso, now + MINUTE)).toBe("1h")
  })
})

describe("groupByFolderWithReuse", () => {
  it("sorts every folder bucket by effective updated time", () => {
    const createdNewer = conv(2, 10, {
      created_at: "2026-07-18T03:00:00.000Z",
      updated_at: "2026-07-18T01:00:00.000Z",
    })
    const activeNewer = conv(1, 10, {
      created_at: "2026-07-18T01:00:00.000Z",
      updated_at: "2026-07-18T02:00:00.000Z",
    })
    const optimistic = new Map([
      [
        2,
        {
          token: "t2",
          baselineUpdatedAt: createdNewer.updated_at,
          effectiveAt: "2026-07-18T04:00:00.000Z",
        },
      ],
    ])

    const grouped = groupByFolderWithReuse(
      [createdNewer, activeNewer],
      new Map(),
      undefined,
      optimistic
    )
    expect(grouped.get(10)!.map((row) => row.id)).toEqual([2, 1])
  })

  it("tie-breaks equal effective updated time by created_at then id", () => {
    const sameUpdated = "2026-07-18T05:00:00.000Z"
    // Higher created_at wins when updated_at ties.
    const olderCreated = conv(1, 10, {
      created_at: "2026-07-18T01:00:00.000Z",
      updated_at: sameUpdated,
    })
    const newerCreated = conv(2, 10, {
      created_at: "2026-07-18T02:00:00.000Z",
      updated_at: sameUpdated,
    })
    expect(
      groupByFolderWithReuse([olderCreated, newerCreated], new Map())
        .get(10)!
        .map((c) => c.id)
    ).toEqual([2, 1])

    // Same created_at + updated_at → higher id wins.
    const lowId = conv(10, 20, {
      created_at: "2026-07-18T01:00:00.000Z",
      updated_at: sameUpdated,
    })
    const highId = conv(20, 20, {
      created_at: "2026-07-18T01:00:00.000Z",
      updated_at: sameUpdated,
    })
    expect(
      groupByFolderWithReuse([lowId, highId], new Map())
        .get(20)!
        .map((c) => c.id)
    ).toEqual([20, 10])
  })

  it("reuses the prior bucket array for folders whose membership is unchanged", () => {
    const a1 = conv(1, 10)
    const a2 = conv(2, 10)
    const b1 = conv(3, 20)
    const first = groupByFolderWithReuse([a1, a2, b1], new Map())

    // Simulate a status event on folder 10: one summary is replaced by a new
    // object (slice + spread), every other summary keeps its identity.
    const a2Patched = { ...a2, status: "completed" as const }
    const second = groupByFolderWithReuse([a1, a2Patched, b1], first)

    // Folder 20 is untouched → same array reference (memo can bail out).
    expect(second.get(20)).toBe(first.get(20))
    // Folder 10 changed → a fresh array reference.
    expect(second.get(10)).not.toBe(first.get(10))
    // …but the untouched summary inside folder 10 keeps its object identity,
    // so its card memo still bails out.
    expect(second.get(10)).toContain(a1)
    expect(second.get(10)).toContain(a2Patched)
    expect(second.get(10)).not.toContain(a2)
  })

  it("reuses every bucket when nothing changed at all", () => {
    const list = [conv(1, 10), conv(2, 20)]
    const first = groupByFolderWithReuse(list, new Map())
    const second = groupByFolderWithReuse(list, first)
    expect(second.get(10)).toBe(first.get(10))
    expect(second.get(20)).toBe(first.get(20))
  })

  it("merges worktree child folders into their parent bucket", () => {
    // folder 11 + 12 are worktrees of root folder 10.
    const childToParent = new Map<number, number>([
      [11, 10],
      [12, 10],
    ])
    const list = [conv(1, 10), conv(2, 11), conv(3, 12), conv(4, 20)]
    const grouped = groupByFolderWithReuse(list, new Map(), childToParent)

    // No child folder gets its own bucket; everything lands under the root (10).
    expect([...grouped.keys()].sort((a, b) => a - b)).toEqual([10, 20])
    expect(
      grouped
        .get(10)!
        .map((c) => c.id)
        .sort()
    ).toEqual([1, 2, 3])
    // The merge never rewrites folder_id — each conversation keeps its own.
    const merged = grouped.get(10)!
    expect(merged.find((c) => c.id === 2)!.folder_id).toBe(11)
    expect(merged.find((c) => c.id === 3)!.folder_id).toBe(12)
  })

  it("sorts the merged parent+worktree bucket by effective updated time", () => {
    const childToParent = new Map<number, number>([[11, 10]])
    // Effective timestamps interleaved across root folder and worktree:
    // id 4 (worktree) most recent, then 3 (root), 2 (worktree), 1 (root).
    const list = [
      conv(1, 10, { updated_at: "2026-07-18T01:00:00.000Z" }),
      conv(4, 11, { updated_at: "2026-07-18T04:00:00.000Z" }),
      conv(2, 11, { updated_at: "2026-07-18T02:00:00.000Z" }),
      conv(3, 10, { updated_at: "2026-07-18T03:00:00.000Z" }),
    ]
    const grouped = groupByFolderWithReuse(list, new Map(), childToParent)
    expect(grouped.get(10)!.map((c) => c.id)).toEqual([4, 3, 2, 1])
  })

  it("leaves grouping unchanged when childToParent is empty/omitted", () => {
    const list = [conv(1, 10), conv(2, 11)]
    const withEmpty = groupByFolderWithReuse(list, new Map(), new Map())
    expect([...withEmpty.keys()].sort((a, b) => a - b)).toEqual([10, 11])
  })
})

describe("reuseSet", () => {
  it("returns the previous set when membership is unchanged", () => {
    const prev = new Set(["a:1", "b:2"])
    const next = new Set(["b:2", "a:1"])
    expect(reuseSet(prev, next)).toBe(prev)
  })

  it("returns the next set when membership differs", () => {
    const prev = new Set(["a:1"])
    expect(reuseSet(prev, new Set(["a:1", "b:2"]))).not.toBe(prev)
    expect(reuseSet(new Set(["a:1", "b:2"]), new Set(["a:1"]))).toEqual(
      new Set(["a:1"])
    )
    expect(reuseSet(new Set(["a:1"]), new Set(["b:2"]))).toEqual(
      new Set(["b:2"])
    )
  })
})

describe("reuseSelected", () => {
  it("returns the previous ref when it denotes the same conversation", () => {
    const prev = { id: 1, agentType: "claude_code" }
    expect(reuseSelected(prev, { id: 1, agentType: "claude_code" })).toBe(prev)
  })

  it("returns the next value when the selection changed or cleared", () => {
    const prev = { id: 1, agentType: "claude_code" }
    expect(reuseSelected(prev, { id: 2, agentType: "claude_code" })).toEqual({
      id: 2,
      agentType: "claude_code",
    })
    expect(reuseSelected(prev, { id: 1, agentType: "codex" })).toEqual({
      id: 1,
      agentType: "codex",
    })
    expect(reuseSelected(prev, null)).toBeNull()
    expect(reuseSelected(null, prev)).toBe(prev)
  })
})

describe("buildRows", () => {
  const foldersHeader = (count: number) =>
    ({ kind: "section", section: "folders", expanded: true, count }) as const

  // Folder-only convenience wrapper (no pinned section), matching the original
  // positional tests but through the new options-object signature. The Chat
  // section is always present now (a permanent entry point), but it is exercised
  // by its own tests below — so this wrapper trims it off to keep the focused
  // folder assertions exact.
  function folderRows(
    orderedFolderIds: number[],
    byFolder: Map<number, DbConversationSummary[]>,
    folderExpanded: Record<number, boolean>,
    folderTotalCounts: Map<number, number>,
    foldersExpanded = true
  ): SidebarRow[] {
    const rows = buildRows({
      pinned: [],
      pinnedExpanded: true,
      orderedFolderIds,
      byFolder,
      folderExpanded,
      folderTotalCounts,
      foldersExpanded,
      chatConversations: [],
      chatsExpanded: true,
    })
    const chatsIdx = rows.findIndex(
      (r) => r.kind === "section" && r.section === "chats"
    )
    return chatsIdx === -1 ? rows : rows.slice(0, chatsIdx)
  }

  it("emits a Folders section header above the folder rows", () => {
    const byFolder = new Map([[10, [conv(1, 10)]]])
    const rows = folderRows([10], byFolder, { 10: true }, new Map([[10, 1]]))
    expect(rows[0]).toEqual(foldersHeader(1))
  })

  it("emits header + a single folder row for a collapsed folder", () => {
    const byFolder = new Map([[10, [conv(1, 10), conv(2, 10)]]])
    const rows = folderRows([10], byFolder, { 10: false }, new Map([[10, 2]]))
    expect(rows).toEqual([foldersHeader(1), { kind: "folder", folderId: 10 }])
  })

  it("defaults to expanded when folderExpanded has no entry", () => {
    const byFolder = new Map([[10, [conv(1, 10)]]])
    const rows = folderRows([10], byFolder, {}, new Map([[10, 1]]))
    expect(rows.map((r) => r.kind)).toEqual([
      "section",
      "folder",
      "conversation",
    ])
  })

  it("emits header + empty-hint row for an expanded folder with no visible rows", () => {
    const rows = folderRows([10], new Map(), { 10: true }, new Map([[10, 3]]))
    expect(rows).toEqual([
      foldersHeader(1),
      { kind: "folder", folderId: 10 },
      { kind: "empty", folderId: 10, totalConversationCount: 3 },
    ])
  })

  it("carries the unfiltered total count on the empty-hint row", () => {
    // byFolder is empty (all filtered out) but the folder has 5 conversations
    // total → renderer shows "no unfinished conversations", not "empty folder".
    const rows = folderRows([10], new Map(), { 10: true }, new Map([[10, 5]]))
    const empty = rows.find((r) => r.kind === "empty")
    expect(empty).toMatchObject({ totalConversationCount: 5 })
  })

  it("emits header + each conversation row, passing summary references through", () => {
    const a = conv(1, 10)
    const b = conv(2, 10)
    const byFolder = new Map([[10, [a, b]]])
    const rows = folderRows([10], byFolder, { 10: true }, new Map([[10, 2]]))
    // [folders-header, folder, conv a, conv b]
    expect(rows).toHaveLength(4)
    expect(rows[1]).toEqual({ kind: "folder", folderId: 10 })
    // The exact summary object references survive (identity, not a copy) — this
    // is what keeps the card memo alive through the flat row model.
    expect(
      (rows[2] as { conversation: DbConversationSummary }).conversation
    ).toBe(a)
    expect(
      (rows[3] as { conversation: DbConversationSummary }).conversation
    ).toBe(b)
  })

  it("follows orderedFolderIds order across multiple folders", () => {
    const byFolder = new Map([
      [10, [conv(1, 10)]],
      [20, [conv(2, 20)]],
    ])
    const expanded = { 10: true, 20: false }
    const counts = new Map([
      [10, 1],
      [20, 1],
    ])
    // Folder 20 first (collapsed → header only), then 10 (expanded).
    const rows = folderRows([20, 10], byFolder, expanded, counts)
    expect(rows).toEqual([
      foldersHeader(2),
      { kind: "folder", folderId: 20 },
      { kind: "folder", folderId: 10 },
      conversationRow(byFolder.get(10)![0], 0, 1, "folder:10"),
    ])
  })

  it("returns an empty array when there are no folders and nothing pinned", () => {
    expect(folderRows([], new Map(), {}, new Map())).toEqual([])
  })

  it("hides every folder row when the Folders section is collapsed", () => {
    const byFolder = new Map([[10, [conv(1, 10)]]])
    const rows = folderRows(
      [10],
      byFolder,
      { 10: true },
      new Map([[10, 1]]),
      false
    )
    expect(rows).toEqual([
      { kind: "section", section: "folders", expanded: false, count: 1 },
    ])
  })

  it("emits a Pinned section above Folders when conversations are pinned", () => {
    const p1 = conv(1, 10, { pinned_at: new Date(2000).toISOString() })
    const byFolder = new Map([[10, [conv(2, 10)]]])
    const rows = buildRows({
      pinned: [p1],
      pinnedExpanded: true,
      orderedFolderIds: [10],
      byFolder,
      folderExpanded: { 10: true },
      folderTotalCounts: new Map([[10, 1]]),
      foldersExpanded: true,
      chatConversations: [],
      chatsExpanded: true,
    })
    expect(rows[0]).toEqual({
      kind: "section",
      section: "pinned",
      expanded: true,
      count: 1,
    })
    expect(rows[1]).toEqual(conversationRow(p1, 0, 1, "pinned"))
    expect(rows[2]).toEqual({
      kind: "section",
      section: "folders",
      expanded: true,
      count: 1,
    })
  })

  it("hides pinned conversations when the Pinned section is collapsed", () => {
    const p1 = conv(1, 10, { pinned_at: new Date(2000).toISOString() })
    const rows = buildRows({
      pinned: [p1],
      pinnedExpanded: false,
      orderedFolderIds: [],
      byFolder: new Map(),
      folderExpanded: {},
      folderTotalCounts: new Map(),
      foldersExpanded: true,
      chatConversations: [],
      chatsExpanded: true,
    })
    // Pinned section collapsed → header only; the always-present Chat section
    // trails (empty → header + hint).
    expect(rows).toEqual([
      { kind: "section", section: "pinned", expanded: false, count: 1 },
      { kind: "section", section: "chats", expanded: true, count: 0 },
      { kind: "chats-empty" },
    ])
  })

  it("omits the Pinned section entirely when nothing is pinned", () => {
    const byFolder = new Map([[10, [conv(1, 10)]]])
    const rows = folderRows([10], byFolder, { 10: true }, new Map([[10, 1]]))
    expect(
      rows.some((r) => r.kind === "section" && r.section === "pinned")
    ).toBe(false)
  })

  it("emits a flat Chat section below the folders section", () => {
    const c1 = conv(1, 99)
    const c2 = conv(2, 99)
    const rows = buildRows({
      pinned: [],
      pinnedExpanded: true,
      orderedFolderIds: [10],
      byFolder: new Map([[10, [conv(3, 10)]]]),
      folderExpanded: { 10: true },
      folderTotalCounts: new Map([[10, 1]]),
      foldersExpanded: true,
      chatConversations: [c1, c2],
      chatsExpanded: true,
    })
    const foldersIdx = rows.findIndex(
      (r) => r.kind === "section" && r.section === "folders"
    )
    const chatsIdx = rows.findIndex(
      (r) => r.kind === "section" && r.section === "chats"
    )
    expect(foldersIdx).toBeGreaterThanOrEqual(0)
    expect(chatsIdx).toBeGreaterThan(foldersIdx)
    expect(rows[chatsIdx]).toEqual({
      kind: "section",
      section: "chats",
      expanded: true,
      count: 2,
    })
    expect(rows[chatsIdx + 1]).toEqual(conversationRow(c1, 0, 1, "chat"))
    expect(rows[chatsIdx + 2]).toEqual(conversationRow(c2, 0, 2, "chat"))
    // Flat — no folder headers inside the chat section.
    expect(rows.slice(chatsIdx + 1).some((r) => r.kind === "folder")).toBe(
      false
    )
  })

  it("places Chat above Folders when sectionOrder is chats-first, keeping Pinned on top", () => {
    const p1 = conv(1, 10, { pinned_at: new Date(2000).toISOString() })
    const rows = buildRows({
      pinned: [p1],
      pinnedExpanded: true,
      orderedFolderIds: [10],
      byFolder: new Map([[10, [conv(2, 10)]]]),
      folderExpanded: { 10: true },
      folderTotalCounts: new Map([[10, 1]]),
      foldersExpanded: true,
      chatConversations: [conv(3, 99)],
      chatsExpanded: true,
      sectionOrder: "chats-first",
    })
    const pinnedIdx = rows.findIndex(
      (r) => r.kind === "section" && r.section === "pinned"
    )
    const chatsIdx = rows.findIndex(
      (r) => r.kind === "section" && r.section === "chats"
    )
    const foldersIdx = rows.findIndex(
      (r) => r.kind === "section" && r.section === "folders"
    )
    // Pinned stays at the very top; Folders and Chat are swapped.
    expect(pinnedIdx).toBe(0)
    expect(chatsIdx).toBeGreaterThan(pinnedIdx)
    expect(foldersIdx).toBeGreaterThan(chatsIdx)
  })

  it("places Folders above Chat when sectionOrder is folders-first (the default)", () => {
    const args = {
      pinned: [] as DbConversationSummary[],
      pinnedExpanded: true,
      orderedFolderIds: [10],
      byFolder: new Map([[10, [conv(1, 10)]]]),
      folderExpanded: { 10: true },
      folderTotalCounts: new Map([[10, 1]]),
      foldersExpanded: true,
      chatConversations: [conv(2, 99)],
      chatsExpanded: true,
    }
    const order = (rows: SidebarRow[]) => {
      const f = rows.findIndex(
        (r) => r.kind === "section" && r.section === "folders"
      )
      const c = rows.findIndex(
        (r) => r.kind === "section" && r.section === "chats"
      )
      return { f, c }
    }
    // Explicit folders-first and the omitted default agree: Folders before Chat.
    const explicit = order(
      buildRows({ ...args, sectionOrder: "folders-first" })
    )
    const omitted = order(buildRows(args))
    expect(explicit.f).toBeGreaterThanOrEqual(0)
    expect(explicit.c).toBeGreaterThan(explicit.f)
    expect(omitted.c).toBeGreaterThan(omitted.f)
  })

  it("always emits the Chat section, with an empty hint when there are no chat conversations", () => {
    const rows = buildRows({
      pinned: [],
      pinnedExpanded: true,
      orderedFolderIds: [10],
      byFolder: new Map([[10, [conv(1, 10)]]]),
      folderExpanded: { 10: true },
      folderTotalCounts: new Map([[10, 1]]),
      foldersExpanded: true,
      chatConversations: [],
      chatsExpanded: true,
    })
    const chatsIdx = rows.findIndex(
      (r) => r.kind === "section" && r.section === "chats"
    )
    // The header is present (count 0) even with no chat conversations — it is a
    // permanent entry point — and an expanded empty section shows a single hint.
    expect(rows[chatsIdx]).toEqual({
      kind: "section",
      section: "chats",
      expanded: true,
      count: 0,
    })
    expect(rows[chatsIdx + 1]).toEqual({ kind: "chats-empty" })
  })

  it("shows only the Chat header (no empty hint) when the empty section is collapsed", () => {
    const rows = buildRows({
      pinned: [],
      pinnedExpanded: true,
      orderedFolderIds: [],
      byFolder: new Map(),
      folderExpanded: {},
      folderTotalCounts: new Map(),
      foldersExpanded: true,
      chatConversations: [],
      chatsExpanded: false,
    })
    expect(rows).toEqual([
      { kind: "section", section: "chats", expanded: false, count: 0 },
    ])
  })

  it("hides chat conversations when the Chat section is collapsed", () => {
    const rows = buildRows({
      pinned: [],
      pinnedExpanded: true,
      orderedFolderIds: [],
      byFolder: new Map(),
      folderExpanded: {},
      folderTotalCounts: new Map(),
      foldersExpanded: true,
      chatConversations: [conv(1, 99)],
      chatsExpanded: false,
    })
    expect(rows).toEqual([
      { kind: "section", section: "chats", expanded: false, count: 1 },
    ])
  })

  // ── Delegation sub-session subtree (recursive expansion) ─────────────────

  it("recurses into an expanded conversation's cached children at depth+1", () => {
    const parent = conv(1, 10, { child_count: 2 })
    const childA = conv(100, 10, { kind: "delegate", parent_id: 1 })
    const childB = conv(101, 10, { kind: "delegate", parent_id: 1 })
    const rows = buildRows({
      pinned: [],
      pinnedExpanded: true,
      orderedFolderIds: [10],
      byFolder: new Map([[10, [parent]]]),
      folderExpanded: { 10: true },
      folderTotalCounts: new Map([[10, 1]]),
      foldersExpanded: true,
      chatConversations: [],
      chatsExpanded: true,
      conversationExpanded: new Set([1]),
      childrenByParent: new Map([[1, [childA, childB]]]),
    })
    expect(rows.filter((r) => r.kind === "conversation")).toEqual([
      conversationRow(parent, 0, 1, "folder:10"),
      conversationRow(childA, 1, 1, "folder:10"),
      conversationRow(childB, 1, 1, "folder:10"),
    ])
  })

  it("does not recurse when the conversation is collapsed", () => {
    const parent = conv(1, 10, { child_count: 2 })
    const childA = conv(100, 10, { parent_id: 1 })
    const rows = buildRows({
      pinned: [],
      pinnedExpanded: true,
      orderedFolderIds: [10],
      byFolder: new Map([[10, [parent]]]),
      folderExpanded: { 10: true },
      folderTotalCounts: new Map([[10, 1]]),
      foldersExpanded: true,
      chatConversations: [],
      chatsExpanded: true,
      conversationExpanded: new Set(),
      childrenByParent: new Map([[1, [childA]]]),
    })
    expect(rows.filter((r) => r.kind === "conversation")).toEqual([
      conversationRow(parent, 0, 1, "folder:10"),
    ])
  })

  it("emits a loading row when expanded but children are not yet fetched", () => {
    const parent = conv(1, 10, { child_count: 3 })
    const rows = buildRows({
      pinned: [],
      pinnedExpanded: true,
      orderedFolderIds: [10],
      byFolder: new Map([[10, [parent]]]),
      folderExpanded: { 10: true },
      folderTotalCounts: new Map([[10, 1]]),
      foldersExpanded: true,
      chatConversations: [],
      chatsExpanded: true,
      conversationExpanded: new Set([1]),
      childrenByParent: new Map(),
    })
    expect(rows).toContainEqual({
      kind: "subsession-loading",
      parentId: 1,
      depth: 1,
      rootId: 1,
      bucketKey: "folder:10",
    })
  })

  it("renders nothing extra when expanded children loaded empty (stale count)", () => {
    const parent = conv(1, 10, { child_count: 1 })
    const rows = buildRows({
      pinned: [],
      pinnedExpanded: true,
      orderedFolderIds: [10],
      byFolder: new Map([[10, [parent]]]),
      folderExpanded: { 10: true },
      folderTotalCounts: new Map([[10, 1]]),
      foldersExpanded: true,
      chatConversations: [],
      chatsExpanded: true,
      conversationExpanded: new Set([1]),
      childrenByParent: new Map([[1, []]]),
    })
    expect(rows.some((r) => r.kind === "subsession-loading")).toBe(false)
    expect(rows.filter((r) => r.kind === "conversation")).toEqual([
      conversationRow(parent, 0, 1, "folder:10"),
    ])
  })

  it("recurses grandchildren when nested conversations are expanded", () => {
    const parent = conv(1, 10, { child_count: 1 })
    const child = conv(100, 10, { child_count: 1, parent_id: 1 })
    const grandchild = conv(200, 10, { parent_id: 100 })
    const rows = buildRows({
      pinned: [],
      pinnedExpanded: true,
      orderedFolderIds: [10],
      byFolder: new Map([[10, [parent]]]),
      folderExpanded: { 10: true },
      folderTotalCounts: new Map([[10, 1]]),
      foldersExpanded: true,
      chatConversations: [],
      chatsExpanded: true,
      conversationExpanded: new Set([1, 100]),
      childrenByParent: new Map([
        [1, [child]],
        [100, [grandchild]],
      ]),
    })
    expect(rows.filter((r) => r.kind === "conversation")).toEqual([
      conversationRow(parent, 0, 1, "folder:10"),
      conversationRow(child, 1, 1, "folder:10"),
      conversationRow(grandchild, 2, 1, "folder:10"),
    ])
  })

  it("passes child summary references through untouched (card memo stability)", () => {
    const parent = conv(1, 10, { child_count: 1 })
    const child = conv(100, 10, { parent_id: 1 })
    const rows = buildRows({
      pinned: [],
      pinnedExpanded: true,
      orderedFolderIds: [10],
      byFolder: new Map([[10, [parent]]]),
      folderExpanded: { 10: true },
      folderTotalCounts: new Map([[10, 1]]),
      foldersExpanded: true,
      chatConversations: [],
      chatsExpanded: true,
      conversationExpanded: new Set([1]),
      childrenByParent: new Map([[1, [child]]]),
    })
    const childRow = rows.find(
      (r) => r.kind === "conversation" && r.conversation.id === 100
    ) as { conversation: DbConversationSummary }
    expect(childRow.conversation).toBe(child)
  })

  it("shows a loading row for an in-flight placeholder (empty array + loading)", () => {
    const parent = conv(1, 10, { child_count: 2 })
    const rows = buildRows({
      pinned: [],
      pinnedExpanded: true,
      orderedFolderIds: [10],
      byFolder: new Map([[10, [parent]]]),
      folderExpanded: { 10: true },
      folderTotalCounts: new Map([[10, 1]]),
      foldersExpanded: true,
      chatConversations: [],
      chatsExpanded: true,
      conversationExpanded: new Set([1]),
      childrenByParent: new Map([[1, []]]),
      childrenLoading: new Set([1]),
    })
    expect(rows).toContainEqual({
      kind: "subsession-loading",
      parentId: 1,
      depth: 1,
      rootId: 1,
      bucketKey: "folder:10",
    })
  })

  it("propagates rootId and bucketKey through folder root blocks", () => {
    const parent = conv(1, 10, { child_count: 1 })
    const child = conv(100, 10, {
      kind: "delegate",
      parent_id: 1,
      child_count: 1,
    })
    const grandchild = conv(101, 10, { kind: "delegate", parent_id: 100 })
    const rows = buildRows({
      pinned: [],
      pinnedExpanded: true,
      orderedFolderIds: [10],
      byFolder: new Map([[10, [parent]]]),
      folderExpanded: { 10: true },
      folderTotalCounts: new Map([[10, 1]]),
      foldersExpanded: true,
      chatConversations: [],
      chatsExpanded: true,
      conversationExpanded: new Set([1, 100]),
      childrenByParent: new Map([
        [1, [child]],
        [100, [grandchild]],
      ]),
    })
    expect(
      rows
        .filter((row) => row.kind === "conversation")
        .map((row) => ({
          id: row.conversation.id,
          rootId: row.rootId,
          bucketKey: row.bucketKey,
          key: sidebarRowKey(row),
        }))
    ).toEqual([
      { id: 1, rootId: 1, bucketKey: "folder:10", key: "conv-claude_code-1" },
      {
        id: 100,
        rootId: 1,
        bucketKey: "folder:10",
        key: "conv-claude_code-100",
      },
      {
        id: 101,
        rootId: 1,
        bucketKey: "folder:10",
        key: "conv-claude_code-101",
      },
    ])
  })

  it("propagates rootId and bucketKey for pinned root blocks", () => {
    const parent = conv(1, 10, {
      child_count: 1,
      pinned_at: "2026-07-18T01:00:00.000Z",
    })
    const child = conv(100, 10, { kind: "delegate", parent_id: 1 })
    const rows = buildRows({
      pinned: [parent],
      pinnedExpanded: true,
      orderedFolderIds: [],
      byFolder: new Map(),
      folderExpanded: {},
      folderTotalCounts: new Map(),
      foldersExpanded: true,
      chatConversations: [],
      chatsExpanded: true,
      conversationExpanded: new Set([1]),
      childrenByParent: new Map([[1, [child]]]),
    })
    expect(
      rows
        .filter((row) => row.kind === "conversation")
        .map((row) => ({
          id: row.conversation.id,
          rootId: row.rootId,
          bucketKey: row.bucketKey,
          key: sidebarRowKey(row),
        }))
    ).toEqual([
      { id: 1, rootId: 1, bucketKey: "pinned", key: "conv-claude_code-1" },
      { id: 100, rootId: 1, bucketKey: "pinned", key: "conv-claude_code-100" },
    ])
  })

  it("propagates rootId and bucketKey for chat root blocks", () => {
    const parent = conv(1, 99, { kind: "chat", child_count: 1 })
    const child = conv(100, 99, { kind: "delegate", parent_id: 1 })
    const rows = buildRows({
      pinned: [],
      pinnedExpanded: true,
      orderedFolderIds: [],
      byFolder: new Map(),
      folderExpanded: {},
      folderTotalCounts: new Map(),
      foldersExpanded: true,
      chatConversations: [parent],
      chatsExpanded: true,
      conversationExpanded: new Set([1]),
      childrenByParent: new Map([[1, [child]]]),
    })
    expect(
      rows
        .filter((row) => row.kind === "conversation")
        .map((row) => ({
          id: row.conversation.id,
          rootId: row.rootId,
          bucketKey: row.bucketKey,
          key: sidebarRowKey(row),
        }))
    ).toEqual([
      { id: 1, rootId: 1, bucketKey: "chat", key: "conv-claude_code-1" },
      { id: 100, rootId: 1, bucketKey: "chat", key: "conv-claude_code-100" },
    ])
  })

  it("marks loading placeholders with the owning root's rootId and bucketKey", () => {
    const parent = conv(1, 10, { child_count: 2 })
    const rows = buildRows({
      pinned: [],
      pinnedExpanded: true,
      orderedFolderIds: [10],
      byFolder: new Map([[10, [parent]]]),
      folderExpanded: { 10: true },
      folderTotalCounts: new Map([[10, 1]]),
      foldersExpanded: true,
      chatConversations: [],
      chatsExpanded: true,
      conversationExpanded: new Set([1]),
      childrenByParent: new Map(),
    })
    const loading = rows.find((r) => r.kind === "subsession-loading")
    expect(loading).toEqual({
      kind: "subsession-loading",
      parentId: 1,
      depth: 1,
      rootId: 1,
      bucketKey: "folder:10",
    })
    expect(sidebarRowKey(loading!)).toBe("subloading-1")
  })
})

describe("sidebarRowKey", () => {
  it("preserves every existing key string form", () => {
    const c = conv(1, 10)
    expect(
      sidebarRowKey({
        kind: "section",
        section: "pinned",
        expanded: true,
        count: 1,
      })
    ).toBe("section-pinned")
    expect(sidebarRowKey({ kind: "folder", folderId: 10 })).toBe("folder-10")
    expect(
      sidebarRowKey({
        kind: "empty",
        folderId: 10,
        totalConversationCount: 0,
      })
    ).toBe("empty-10")
    expect(sidebarRowKey({ kind: "chats-empty" })).toBe("chats-empty")
    expect(
      sidebarRowKey({
        kind: "subsession-loading",
        parentId: 1,
        depth: 1,
        rootId: 1,
        bucketKey: "folder:10",
      })
    ).toBe("subloading-1")
    expect(
      sidebarRowKey(
        conversationRow(c, 0, 1, "folder:10") as Extract<
          SidebarRow,
          { kind: "conversation" }
        >
      )
    ).toBe("conv-claude_code-1")
  })
})

describe("mergeChildrenById", () => {
  it("keeps live events over the snapshot by id and adds new children", () => {
    const snapA = conv(100, 1, { status: "pending" })
    const snapB = conv(102, 1)
    const eventA = conv(100, 1, { status: "completed" }) // newer status, same id
    const eventC = conv(101, 1) // new child absent from the snapshot
    const merged = mergeChildrenById([snapA, snapB], [eventA, eventC])
    // created_at descending / newest-first (the factory derives created_at from
    // id, so higher id == newer)
    expect(merged.map((c) => c.id)).toEqual([102, 101, 100])
    // the live event wins over the snapshot for the shared id
    expect(merged.find((c) => c.id === 100)!.status).toBe("completed")
  })

  it("sorts the snapshot newest-first when nothing is buffered", () => {
    const snap = [conv(100, 1), conv(101, 1)]
    expect(mergeChildrenById(snap, []).map((c) => c.id)).toEqual([101, 100])
  })
})

describe("selectChatConversationsWithReuse", () => {
  it("selects only chat-kind conversations, newest-updated first, excluding pinned", () => {
    const a = conv(1, 99, { kind: "chat" })
    const b = conv(2, 99, { kind: "chat" }) // higher id → later updated_at
    const pinnedChat = conv(3, 99, {
      kind: "chat",
      pinned_at: new Date(5000).toISOString(),
    })
    const folderConv = conv(4, 10)
    const out = selectChatConversationsWithReuse(
      [a, b, pinnedChat, folderConv],
      true,
      []
    )
    expect(out.map((c) => c.id)).toEqual([2, 1])
  })

  it("orders chat roots by optimistic effective updated time", () => {
    const olderActive = conv(1, 99, {
      kind: "chat",
      updated_at: "2026-07-18T02:00:00.000Z",
    })
    const newerCreated = conv(2, 99, {
      kind: "chat",
      updated_at: "2026-07-18T01:00:00.000Z",
    })
    const optimistic = new Map([
      [
        2,
        {
          token: "t2",
          baselineUpdatedAt: newerCreated.updated_at,
          effectiveAt: "2026-07-18T03:00:00.000Z",
        },
      ],
    ])
    expect(
      selectChatConversationsWithReuse(
        [olderActive, newerCreated],
        true,
        [],
        optimistic
      ).map((c) => c.id)
    ).toEqual([2, 1])
  })

  it("excludes completed conversations unless showCompleted", () => {
    const done = conv(1, 99, { kind: "chat", status: "completed" })
    const active = conv(2, 99, { kind: "chat" })
    expect(
      selectChatConversationsWithReuse([done, active], false, []).map(
        (c) => c.id
      )
    ).toEqual([2])
    expect(
      selectChatConversationsWithReuse([done, active], true, [])
        .map((c) => c.id)
        .sort()
    ).toEqual([1, 2])
  })

  it("returns the prev array when membership is referentially unchanged", () => {
    const a = conv(1, 99, { kind: "chat" })
    const first = selectChatConversationsWithReuse([a], true, [])
    const second = selectChatConversationsWithReuse([a], true, first)
    expect(second).toBe(first)
  })
})

describe("selectPinnedWithReuse", () => {
  it("sorts pinned roots by activity before pinned_at", () => {
    const olderPinButActive = conv(1, 10, {
      pinned_at: "2026-07-18T01:00:00.000Z",
      updated_at: "2026-07-18T04:00:00.000Z",
    })
    const newerPin = conv(2, 10, {
      pinned_at: "2026-07-18T03:00:00.000Z",
      updated_at: "2026-07-18T02:00:00.000Z",
    })
    expect(
      selectPinnedWithReuse([newerPin, olderPinButActive], [], new Map()).map(
        (row) => row.id
      )
    ).toEqual([1, 2])
  })

  it("tie-breaks equal effective updated time by pinned_at then id", () => {
    const sameUpdated = "2026-07-18T05:00:00.000Z"
    const olderPin = conv(1, 10, {
      pinned_at: "2026-07-18T01:00:00.000Z",
      updated_at: sameUpdated,
    })
    const newerPin = conv(2, 10, {
      pinned_at: "2026-07-18T03:00:00.000Z",
      updated_at: sameUpdated,
    })
    expect(
      selectPinnedWithReuse([olderPin, newerPin], []).map((p) => p.id)
    ).toEqual([2, 1])

    const lowId = conv(10, 10, {
      pinned_at: "2026-07-18T01:00:00.000Z",
      updated_at: sameUpdated,
    })
    const highId = conv(20, 10, {
      pinned_at: "2026-07-18T01:00:00.000Z",
      updated_at: sameUpdated,
    })
    expect(selectPinnedWithReuse([lowId, highId], []).map((p) => p.id)).toEqual(
      [20, 10]
    )
  })

  it("reuses the previous array when pinned membership is unchanged", () => {
    const a = conv(1, 10, { pinned_at: new Date(1000).toISOString() })
    const first = selectPinnedWithReuse([a], [])
    const second = selectPinnedWithReuse([a], first)
    expect(second).toBe(first)
  })

  it("returns a fresh array when a conversation is pinned or unpinned", () => {
    const a = conv(1, 10, {
      pinned_at: "2026-07-18T01:00:00.000Z",
      updated_at: "2026-07-18T01:00:00.000Z",
    })
    const b = conv(2, 10) // unpinned
    const first = selectPinnedWithReuse([a, b], [])
    const bPinned = {
      ...b,
      pinned_at: "2026-07-18T02:00:00.000Z",
      updated_at: "2026-07-18T03:00:00.000Z",
    }
    const second = selectPinnedWithReuse([a, bPinned], first)
    expect(second).not.toBe(first)
    // b more recently active → first, then a
    expect(second.map((p) => p.id)).toEqual([2, 1])
  })
})

describe("flatIndexOfConversation", () => {
  const rows: SidebarRow[] = [
    { kind: "folder", folderId: 10 },
    conversationRow(conv(1, 10), 0, 1, "folder:10"),
    conversationRow(conv(2, 10, { agent_type: "codex" }), 0, 2, "folder:10"),
    { kind: "folder", folderId: 20 },
    { kind: "empty", folderId: 20, totalConversationCount: 0 },
  ]

  it("returns the flat index of the matching conversation row", () => {
    expect(flatIndexOfConversation(rows, 1, "claude_code")).toBe(1)
    expect(flatIndexOfConversation(rows, 2, "codex")).toBe(2)
  })

  it("requires both id and agent_type to match", () => {
    expect(flatIndexOfConversation(rows, 2, "claude_code")).toBe(-1)
    expect(flatIndexOfConversation(rows, 99, "claude_code")).toBe(-1)
  })
})

describe("pointerYToTargetIndex", () => {
  it("maps a pointer offset to the row under it", () => {
    // surfaceTop=100, scrollTop=0, rowHeight=32 → y=148 lands in row 1 (132..164)
    expect(pointerYToTargetIndex(148, 100, 0, 32, 5)).toBe(1)
    expect(pointerYToTargetIndex(100, 100, 0, 32, 5)).toBe(0)
  })

  it("accounts for scroll offset", () => {
    // Scrolled down 64px → the same screen Y points two rows lower.
    expect(pointerYToTargetIndex(100, 100, 64, 32, 5)).toBe(2)
  })

  it("clamps above and below the surface", () => {
    expect(pointerYToTargetIndex(0, 100, 0, 32, 5)).toBe(0)
    expect(pointerYToTargetIndex(9999, 100, 0, 32, 5)).toBe(4)
  })

  it("is safe for degenerate inputs", () => {
    expect(pointerYToTargetIndex(150, 100, 0, 32, 0)).toBe(0)
    expect(pointerYToTargetIndex(150, 100, 0, 0, 5)).toBe(0)
  })
})

describe("sticky overlay helpers", () => {
  // F10 expanded (2 convs), F20 collapsed, F30 expanded (empty hint).
  const rows: SidebarRow[] = [
    { kind: "folder", folderId: 10 }, // 0
    conversationRow(conv(1, 10), 0, 1, "folder:10"), // 1
    conversationRow(conv(2, 10), 0, 2, "folder:10"), // 2
    { kind: "folder", folderId: 20 }, // 3
    { kind: "folder", folderId: 30 }, // 4
    { kind: "empty", folderId: 30, totalConversationCount: 0 }, // 5
  ]

  describe("buildOwnerHeaderIndex", () => {
    it("maps every row to the flat index of its owning folder header", () => {
      expect(Array.from(buildOwnerHeaderIndex(rows))).toEqual([
        0, 0, 0, 3, 4, 4,
      ])
    })

    it("returns an empty array for no rows", () => {
      expect(Array.from(buildOwnerHeaderIndex([]))).toEqual([])
    })

    it("treats section headers and pre-folder pinned rows as ownerless (-1)", () => {
      // Pinned section + its conversation precede any folder header, so they
      // must never resolve a folder sticky overlay.
      const withSections: SidebarRow[] = [
        { kind: "section", section: "pinned", expanded: true, count: 1 }, // 0
        conversationRow(conv(5, 10), 0, 5, "pinned"), // 1 (pinned)
        { kind: "section", section: "folders", expanded: true, count: 1 }, // 2
        { kind: "folder", folderId: 10 }, // 3
        conversationRow(conv(1, 10), 0, 1, "folder:10"), // 4
      ]
      expect(Array.from(buildOwnerHeaderIndex(withSections))).toEqual([
        -1, -1, -1, 3, 3,
      ])
    })
  })

  describe("folderHeaderFlatIndices", () => {
    it("lists folder header indices in ascending order", () => {
      expect(folderHeaderFlatIndices(rows)).toEqual([0, 3, 4])
    })

    it("ignores section headers, listing only folder header indices", () => {
      const withSections: SidebarRow[] = [
        { kind: "section", section: "pinned", expanded: true, count: 1 },
        conversationRow(conv(5, 10), 0, 5, "pinned"),
        { kind: "section", section: "folders", expanded: true, count: 2 },
        { kind: "folder", folderId: 10 },
        { kind: "folder", folderId: 20 },
      ]
      expect(folderHeaderFlatIndices(withSections)).toEqual([3, 4])
    })
  })

  describe("nextHeaderAfter", () => {
    it("returns the next header index strictly after the active one", () => {
      const headers = [0, 3, 4]
      expect(nextHeaderAfter(headers, 0)).toBe(3)
      expect(nextHeaderAfter(headers, 3)).toBe(4)
    })

    it("returns null for the last folder", () => {
      expect(nextHeaderAfter([0, 3, 4], 4)).toBeNull()
      expect(nextHeaderAfter([], 0)).toBeNull()
    })
  })

  describe("headerIndexForFolder", () => {
    it("finds the header row index for a folder id", () => {
      expect(headerIndexForFolder(rows, 10)).toBe(0)
      expect(headerIndexForFolder(rows, 30)).toBe(4)
    })

    it("returns -1 when the folder has no header row", () => {
      expect(headerIndexForFolder(rows, 999)).toBe(-1)
    })
  })

  describe("computeStickyState", () => {
    const base = {
      activeHeaderOffset: 0,
      nextHeaderOffset: 96,
      headerHeight: 32,
    }

    it("hides the overlay when the real header is at the top", () => {
      expect(computeStickyState({ ...base, scrollOffset: 0 })).toEqual({
        visible: false,
        translateY: 0,
      })
    })

    it("shows the overlay with no offset mid-folder", () => {
      expect(computeStickyState({ ...base, scrollOffset: 40 })).toEqual({
        visible: true,
        translateY: 0,
      })
    })

    it("pushes the overlay up as the next header enters the handoff window", () => {
      // next header at 96, scrolled to 80 → d=16 (<32) → translateY 16-32 = -16
      expect(computeStickyState({ ...base, scrollOffset: 80 })).toEqual({
        visible: true,
        translateY: -16,
      })
    })

    it("does not push while the next header is a full header height away", () => {
      // d === headerHeight is the exclusive boundary → no push yet.
      expect(computeStickyState({ ...base, scrollOffset: 64 })).toEqual({
        visible: true,
        translateY: 0,
      })
    })

    it("never pushes for the last folder (no next header)", () => {
      expect(
        computeStickyState({
          scrollOffset: 1000,
          activeHeaderOffset: 320,
          nextHeaderOffset: null,
          headerHeight: 32,
        })
      ).toEqual({ visible: true, translateY: 0 })
    })

    it("rounds the handoff offset to whole pixels", () => {
      // d = 95.4 - 80 = 15.4 → round(15.4 - 32) = round(-16.6) = -17
      expect(
        computeStickyState({
          scrollOffset: 80,
          activeHeaderOffset: 0,
          nextHeaderOffset: 95.4,
          headerHeight: 32,
        }).translateY
      ).toBe(-17)
    })
  })
})

describe("applyReorder", () => {
  it("moves an item forward", () => {
    expect(applyReorder([1, 2, 3, 4], 0, 2)).toEqual([2, 3, 1, 4])
  })

  it("moves an item backward", () => {
    expect(applyReorder([1, 2, 3, 4], 3, 1)).toEqual([1, 4, 2, 3])
  })

  it("returns a fresh copy on a no-op move", () => {
    const order = [1, 2, 3]
    const result = applyReorder(order, 1, 1)
    expect(result).toEqual([1, 2, 3])
    expect(result).not.toBe(order)
  })

  it("clamps the destination and ignores an out-of-range source", () => {
    expect(applyReorder([1, 2, 3], 0, 99)).toEqual([2, 3, 1])
    expect(applyReorder([1, 2, 3], 5, 0)).toEqual([1, 2, 3])
  })
})
