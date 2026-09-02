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
  RECENT_PAGE_SIZE,
  reuseSelected,
  reuseSet,
  selectChatConversationsWithReuse,
  selectPinnedWithReuse,
  sidebarRowKey,
  type SidebarBucketKey,
  selectRecentConversationsWithReuse,
  worktreeChildrenByParent,
  worktreeHeaderAlias,
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
    auto_title_finalized: false,
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
    // created_at order is opposite of updated_at so a created-first
    // regression cannot accidentally pass the non-optimistic assertion.
    const createdNewer = conv(2, 10, {
      created_at: "2026-07-18T03:00:00.000Z",
      updated_at: "2026-07-18T01:00:00.000Z",
    })
    const activeNewer = conv(1, 10, {
      created_at: "2026-07-18T01:00:00.000Z",
      updated_at: "2026-07-18T02:00:00.000Z",
    })

    // Non-optimistic: updated_at wins → activeNewer first (created order
    // would put createdNewer first).
    expect(
      groupByFolderWithReuse([createdNewer, activeNewer], new Map())
        .get(10)!
        .map((row) => row.id)
    ).toEqual([1, 2])

    // Optimistic overlay then reverses the pair.
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
    expect(
      groupByFolderWithReuse(
        [createdNewer, activeNewer],
        new Map(),
        undefined,
        optimistic
      )
        .get(10)!
        .map((row) => row.id)
    ).toEqual([2, 1])
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
    // Effective timestamps interleaved across root folder and worktree.
    // created_at is deliberately the reverse of updated_at so a created-first
    // comparator cannot pass: created order would be [1,2,3,4]; updated is
    // [4,3,2,1] (id 4 worktree newest-updated, then 3 root, 2 worktree, 1 root).
    const list = [
      conv(1, 10, {
        created_at: "2026-07-18T04:00:00.000Z",
        updated_at: "2026-07-18T01:00:00.000Z",
      }),
      conv(4, 11, {
        created_at: "2026-07-18T01:00:00.000Z",
        updated_at: "2026-07-18T04:00:00.000Z",
      }),
      conv(2, 11, {
        created_at: "2026-07-18T03:00:00.000Z",
        updated_at: "2026-07-18T02:00:00.000Z",
      }),
      conv(3, 10, {
        created_at: "2026-07-18T02:00:00.000Z",
        updated_at: "2026-07-18T03:00:00.000Z",
      }),
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

describe("worktreeChildrenByParent", () => {
  const folder = (
    id: number,
    parent_id: number | null,
    sort_order = 0,
    name = `folder-${id}`
  ) => ({ id, parent_id, sort_order, name })

  it("returns an empty map when no repo has worktree children", () => {
    const folders = [folder(10, null), folder(20, null)]
    expect(worktreeChildrenByParent([10, 20], folders).size).toBe(0)
  })

  it("groups each repo's open worktree children under it", () => {
    const folders = [
      folder(10, null),
      folder(12, 10, 2, "beta"),
      folder(11, 10, 1, "alpha"),
      folder(20, null),
      folder(21, 20, 0, "wt"),
    ]
    const map = worktreeChildrenByParent([10, 20], folders)
    // 11 (sort_order 1) before 12 (sort_order 2).
    expect(map.get(10)).toEqual([11, 12])
    expect(map.get(20)).toEqual([21])
    // Only repos WITH children are keys.
    expect([...map.keys()].sort((a, b) => a - b)).toEqual([10, 20])
  })

  it("orders children by sort_order, then name, then id", () => {
    const folders = [
      folder(10, null),
      folder(13, 10, 0, "b"),
      folder(12, 10, 0, "a"),
      folder(11, 10, 0, "a"),
    ]
    // equal sort_order → by name ("a" < "b"): 11/12 before 13; tie on name → id.
    expect(worktreeChildrenByParent([10], folders).get(10)).toEqual([
      11, 12, 13,
    ])
  })

  it("omits an orphan worktree whose parent is not a top-level entry", () => {
    // Parent 10 is closed → absent from the top-level set, so worktree 11 is
    // nobody's child here and no container is produced.
    const folders = [folder(11, 10), folder(20, null)]
    expect(worktreeChildrenByParent([11, 20], folders).size).toBe(0)
  })

  it("does not mutate the inputs", () => {
    const folders = [folder(10, null), folder(11, 10)]
    const top = [10]
    worktreeChildrenByParent(top, folders)
    expect(top).toEqual([10])
    expect(folders.map((f) => f.id)).toEqual([10, 11])
  })
})

describe("worktreeHeaderAlias", () => {
  it("prefers the alias — where the seeded branch name lands", () => {
    expect(worktreeHeaderAlias("task/49", "task/49")).toBe("task/49")
  })

  it("lets a renamed worktree win over its branch", () => {
    expect(worktreeHeaderAlias("Payment rewrite", "task/49")).toBe(
      "Payment rewrite"
    )
  })

  it("falls back to the branch", () => {
    expect(worktreeHeaderAlias(null, "task/49")).toBe("task/49")
  })

  it("gives up rather than inventing one, leaving the bare directory name", () => {
    expect(worktreeHeaderAlias(null, null)).toBeNull()
    // Blank-but-present values (a cleared alias round-tripping as "") must not
    // win the chain and render `[ … ]` against an empty label.
    expect(worktreeHeaderAlias("  ", "  ")).toBeNull()
    expect(worktreeHeaderAlias(undefined, undefined)).toBeNull()
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

/** The section headers a row list emits, top to bottom. */
function sectionSequence(rows: readonly SidebarRow[]): string[] {
  return rows.flatMap((r) => (r.kind === "section" ? [r.section] : []))
}

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

  it("emits the Folders header + empty hint when there are no open folders", () => {
    // folderRows trims the trailing Chat section, so this is just the Folders
    // portion: the header is always present (a permanent entry point) and an
    // expanded empty section shows a single folders-empty hint.
    expect(folderRows([], new Map(), {}, new Map())).toEqual([
      foldersHeader(0),
      { kind: "folders-empty" },
    ])
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
    // Pinned section collapsed → header only; the always-present Folders and Chat
    // sections trail (both empty → header + hint).
    expect(rows).toEqual([
      { kind: "section", section: "pinned", expanded: false, count: 1 },
      { kind: "section", section: "folders", expanded: true, count: 0 },
      { kind: "folders-empty" },
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

  it("follows sectionOrder for Folders/Chat/Recent, keeping Pinned on top", () => {
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
      recentConversations: [conv(2, 10)],
      showRecent: true,
      sectionOrder: ["recent", "chats", "folders"],
    })
    // Pinned stays at the very top; the other three follow the given order.
    expect(sectionSequence(rows)).toEqual([
      "pinned",
      "recent",
      "chats",
      "folders",
    ])
  })

  it("places Folders above Chat by default, with no Recent section at all", () => {
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
    // The explicit default order and the omitted one agree — and neither emits
    // a Recent section, since `showRecent` is off unless a caller opts in.
    expect(sectionSequence(buildRows(args))).toEqual(["folders", "chats"])
    expect(
      sectionSequence(
        buildRows({ ...args, sectionOrder: ["folders", "chats", "recent"] })
      )
    ).toEqual(["folders", "chats"])
  })

  it("normalizes a partial or repeated sectionOrder into a full permutation", () => {
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
      recentConversations: [conv(1, 10)],
      showRecent: true,
      // Repeated entry + a missing section: "recent" is emitted once, up front,
      // and the omitted sections fall in behind it in default order.
      sectionOrder: ["recent", "recent"],
    })
    expect(sectionSequence(rows)).toEqual(["recent", "folders", "chats"])
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
      // Folders collapsed too, so this test stays focused on the Chat section:
      // both sections show only their header (no empty hint) when collapsed.
      foldersExpanded: false,
      chatConversations: [],
      chatsExpanded: false,
    })
    expect(rows).toEqual([
      { kind: "section", section: "folders", expanded: false, count: 0 },
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
      foldersExpanded: false,
      chatConversations: [conv(1, 99)],
      chatsExpanded: false,
    })
    expect(rows).toEqual([
      { kind: "section", section: "folders", expanded: false, count: 0 },
      { kind: "section", section: "chats", expanded: false, count: 1 },
    ])
  })

  it("always emits the Folders section, with an empty hint when no folders are open", () => {
    // Mirrors the Chat section: with chats present but no open folders, the
    // Folders header + a single folders-empty hint still render. (The fully-empty
    // initial workspace — no folders AND no conversations — is handled by the
    // list's open-folder call-to-action, not buildRows.)
    const rows = buildRows({
      pinned: [],
      pinnedExpanded: true,
      orderedFolderIds: [],
      byFolder: new Map(),
      folderExpanded: {},
      folderTotalCounts: new Map(),
      foldersExpanded: true,
      chatConversations: [conv(1, 99)],
      chatsExpanded: true,
    })
    const foldersIdx = rows.findIndex(
      (r) => r.kind === "section" && r.section === "folders"
    )
    expect(rows[foldersIdx]).toEqual({
      kind: "section",
      section: "folders",
      expanded: true,
      count: 0,
    })
    expect(rows[foldersIdx + 1]).toEqual({ kind: "folders-empty" })
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

  it("assigns the parent display bucketKey to conversations from a raw worktree folder", () => {
    // Worktree folder 11 merges into parent display bucket 10; the summary
    // keeps folder_id=11 but the flattened row's bucketKey must be folder:10.
    const rootConv = conv(1, 10, {
      created_at: "2026-07-18T02:00:00.000Z",
      updated_at: "2026-07-18T01:00:00.000Z",
    })
    const worktreeConv = conv(2, 11, {
      created_at: "2026-07-18T01:00:00.000Z",
      updated_at: "2026-07-18T02:00:00.000Z",
    })
    const childToParent = new Map<number, number>([[11, 10]])
    const byFolder = groupByFolderWithReuse(
      [rootConv, worktreeConv],
      new Map(),
      childToParent
    )
    expect(byFolder.get(10)!.map((c) => c.id)).toEqual([2, 1])
    expect(worktreeConv.folder_id).toBe(11)

    const rows = buildRows({
      pinned: [],
      pinnedExpanded: true,
      orderedFolderIds: [10],
      byFolder,
      folderExpanded: { 10: true },
      folderTotalCounts: new Map([[10, 2]]),
      foldersExpanded: true,
      chatConversations: [],
      chatsExpanded: true,
    })
    expect(
      rows
        .filter((row) => row.kind === "conversation")
        .map((row) => ({
          id: row.conversation.id,
          folder_id: row.conversation.folder_id,
          rootId: row.rootId,
          bucketKey: row.bucketKey,
          key: sidebarRowKey(row),
        }))
    ).toEqual([
      {
        id: 2,
        folder_id: 11,
        rootId: 2,
        bucketKey: "folder:10",
        key: "conv-claude_code-2",
      },
      {
        id: 1,
        folder_id: 10,
        rootId: 1,
        bucketKey: "folder:10",
        key: "conv-claude_code-1",
      },
    ])
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
    expect(
      sidebarRowKey({ kind: "folder-group", groupId: 7, expanded: true })
    ).toBe("foldergroup-7")
    expect(sidebarRowKey({ kind: "group-empty", groupId: 7 })).toBe(
      "groupempty-7"
    )
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

describe("buildRows — Recent section", () => {
  const baseArgs = {
    pinned: [] as DbConversationSummary[],
    pinnedExpanded: true,
    orderedFolderIds: [10],
    byFolder: new Map([[10, [conv(1, 10)]]]),
    folderExpanded: { 10: true },
    folderTotalCounts: new Map([[10, 1]]),
    foldersExpanded: true,
    chatConversations: [] as DbConversationSummary[],
    chatsExpanded: true,
  }

  it("emits no rows at all — not even a header — when showRecent is off", () => {
    const rows = buildRows({
      ...baseArgs,
      recentConversations: [conv(1, 10)],
      showRecent: false,
    })
    expect(sectionSequence(rows)).toEqual(["folders", "chats"])
    expect(rows.some((r) => r.kind === "recent-empty")).toBe(false)
  })

  it("tags its conversation rows `recent` so they don't collide with their twins", () => {
    const c1 = conv(1, 10)
    const rows = buildRows({
      ...baseArgs,
      recentConversations: [c1],
      showRecent: true,
    })
    const matches = rows.filter(
      (r) => r.kind === "conversation" && r.conversation.id === 1
    )
    // The same conversation appears twice: once under its folder (untagged),
    // once in Recent (tagged).
    expect(matches).toEqual([
      conversationRow(c1, 0, 1, "folder:10"),
      { ...conversationRow(c1, 0, 1, "folder:10"), recent: true },
    ])
  })

  it("tags a Recent parent's lazy-load placeholder and subtree too", () => {
    const parent = conv(1, 10, { child_count: 1 })
    const kid = conv(2, 10)
    const rows = buildRows({
      ...baseArgs,
      byFolder: new Map([[10, [parent]]]),
      recentConversations: [parent],
      showRecent: true,
      conversationExpanded: new Set([1]),
      childrenByParent: new Map([[1, [kid]]]),
    })
    // Child rows inherit the flag, otherwise the folder copy and the Recent
    // copy of the same child would share a React key.
    expect(
      rows.filter((r) => r.kind === "conversation" && r.conversation.id === 2)
    ).toEqual([
      conversationRow(kid, 1, 1, "folder:10"),
      { ...conversationRow(kid, 1, 1, "folder:10"), recent: true },
    ])

    const loadingRows = buildRows({
      ...baseArgs,
      byFolder: new Map([[10, [parent]]]),
      recentConversations: [parent],
      showRecent: true,
      conversationExpanded: new Set([1]),
      childrenByParent: new Map([[1, []]]),
      childrenLoading: new Set([1]),
    }).filter((r) => r.kind === "subsession-loading")
    expect(loadingRows).toEqual([
      {
        kind: "subsession-loading",
        parentId: 1,
        depth: 1,
        rootId: 1,
        bucketKey: "folder:10",
      },
      {
        kind: "subsession-loading",
        parentId: 1,
        depth: 1,
        rootId: 1,
        bucketKey: "folder:10",
        recent: true,
      },
    ])
  })

  it("emits an empty hint when shown with nothing in it", () => {
    const rows = buildRows({
      ...baseArgs,
      recentConversations: [],
      showRecent: true,
    })
    expect(rows).toContainEqual({ kind: "recent-empty" })
  })

  it("emits only its header while collapsed", () => {
    const rows = buildRows({
      ...baseArgs,
      recentConversations: [conv(1, 10)],
      recentExpanded: false,
      showRecent: true,
    })
    expect(rows).toContainEqual({
      kind: "section",
      section: "recent",
      expanded: false,
      count: 1,
    })
    expect(rows.filter((r) => r.kind === "conversation" && r.recent)).toEqual(
      []
    )
  })

  describe("paging", () => {
    const many = Array.from({ length: 5 }, (_, i) => conv(i + 1, 10))

    it("stops at recentLimit and appends a show-more row with the remainder", () => {
      const rows = buildRows({
        ...baseArgs,
        byFolder: new Map([[10, many]]),
        folderTotalCounts: new Map([[10, many.length]]),
        recentConversations: many,
        showRecent: true,
        recentLimit: 2,
      })
      expect(
        rows.filter((r) => r.kind === "conversation" && r.recent).length
      ).toBe(2)
      expect(rows).toContainEqual({ kind: "recent-more", remaining: 3 })
      // The header keeps reporting the TOTAL, not the visible slice.
      expect(rows).toContainEqual({
        kind: "section",
        section: "recent",
        expanded: true,
        count: 5,
      })
    })

    it("omits the show-more row once the limit covers everything", () => {
      const rows = buildRows({
        ...baseArgs,
        byFolder: new Map([[10, many]]),
        folderTotalCounts: new Map([[10, many.length]]),
        recentConversations: many,
        showRecent: true,
        recentLimit: 5,
      })
      expect(rows.some((r) => r.kind === "recent-more")).toBe(false)
    })

    it("emits every conversation when no limit is given", () => {
      const rows = buildRows({
        ...baseArgs,
        byFolder: new Map([[10, many]]),
        folderTotalCounts: new Map([[10, many.length]]),
        recentConversations: many,
        showRecent: true,
      })
      expect(
        rows.filter((r) => r.kind === "conversation" && r.recent).length
      ).toBe(5)
      expect(rows.some((r) => r.kind === "recent-more")).toBe(false)
    })

    it("does not page a collapsed section", () => {
      const rows = buildRows({
        ...baseArgs,
        byFolder: new Map([[10, many]]),
        folderTotalCounts: new Map([[10, many.length]]),
        recentConversations: many,
        recentExpanded: false,
        showRecent: true,
        recentLimit: 2,
      })
      expect(rows.some((r) => r.kind === "recent-more")).toBe(false)
    })

    describe("resetting", () => {
      // Enough to page twice over: one full first page, plus a second one.
      const lots = Array.from({ length: RECENT_PAGE_SIZE + 4 }, (_, i) =>
        conv(i + 1, 10)
      )
      const pagedArgs = {
        ...baseArgs,
        byFolder: new Map([[10, lots]]),
        folderTotalCounts: new Map([[10, lots.length]]),
        recentConversations: lots,
        showRecent: true,
      }

      it("leaves the first page un-resettable", () => {
        const rows = buildRows({ ...pagedArgs, recentLimit: RECENT_PAGE_SIZE })
        // Exact equality: nothing to fold back yet, so no `canReset` at all.
        expect(rows).toContainEqual({ kind: "recent-more", remaining: 4 })
      })

      it("marks the footer resettable once past the first page", () => {
        const rows = buildRows({
          ...pagedArgs,
          recentLimit: RECENT_PAGE_SIZE + 2,
        })
        expect(rows).toContainEqual({
          kind: "recent-more",
          remaining: 2,
          canReset: true,
        })
      })

      it("keeps the footer alive after the last page, as a reset-only row", () => {
        // The regression this guards: retiring the row at `remaining === 0`
        // took the only way back to a short list away exactly when the list was
        // longest.
        const rows = buildRows({
          ...pagedArgs,
          recentLimit: RECENT_PAGE_SIZE * 2,
        })
        expect(
          rows.filter((r) => r.kind === "conversation" && r.recent).length
        ).toBe(lots.length)
        expect(rows).toContainEqual({
          kind: "recent-more",
          remaining: 0,
          canReset: true,
        })
      })

      it("drops the reset once a raised limit outlives the rows it revealed", () => {
        // Same raised limit, but the conversations are gone: collapsing back
        // would hide nothing, so the row must not offer it.
        const rows = buildRows({
          ...pagedArgs,
          byFolder: new Map([[10, many]]),
          folderTotalCounts: new Map([[10, many.length]]),
          recentConversations: many,
          recentLimit: RECENT_PAGE_SIZE * 2,
        })
        expect(rows.some((r) => r.kind === "recent-more")).toBe(false)
      })
    })
  })
})

describe("buildRows — Show worktrees container tree", () => {
  // Trim the trailing (always-present) Chat section for exact folder assertions.
  const trimChats = (rows: SidebarRow[]): SidebarRow[] => {
    const i = rows.findIndex(
      (r) => r.kind === "section" && r.section === "chats"
    )
    return i === -1 ? rows : rows.slice(0, i)
  }

  it("nests a container repo's root sub-group + worktrees, both at depth 1", () => {
    const rootConv = conv(1, 10)
    const wtConv = conv(2, 11)
    const rows = buildRows({
      pinned: [],
      pinnedExpanded: true,
      orderedFolderIds: [10],
      byFolder: new Map([
        [10, [rootConv]],
        [11, [wtConv]],
      ]),
      folderExpanded: {},
      folderTotalCounts: new Map([
        [10, 1],
        [11, 1],
      ]),
      foldersExpanded: true,
      chatConversations: [],
      chatsExpanded: true,
      containerChildren: new Map([[10, [11]]]),
    })
    expect(trimChats(rows)).toEqual([
      { kind: "section", section: "folders", expanded: true, count: 1 },
      { kind: "folder", folderId: 10 }, // container
      { kind: "root-group", folderId: 10 }, // repo's own sessions
      {
        kind: "conversation",
        conversation: rootConv,
        depth: 1,
        rootId: rootConv.id,
        bucketKey: "folder:10",
      },
      { kind: "folder", folderId: 11 }, // worktree
      {
        kind: "conversation",
        conversation: wtConv,
        depth: 1,
        rootId: wtConv.id,
        bucketKey: "folder:11",
      },
    ])
  })

  it("hides the whole subtree when the container is collapsed", () => {
    const rows = buildRows({
      pinned: [],
      pinnedExpanded: true,
      orderedFolderIds: [10],
      byFolder: new Map([
        [10, [conv(1, 10)]],
        [11, [conv(2, 11)]],
      ]),
      folderExpanded: { 10: false },
      folderTotalCounts: new Map(),
      foldersExpanded: true,
      chatConversations: [],
      chatsExpanded: true,
      containerChildren: new Map([[10, [11]]]),
    })
    // Container header only — no root-group, no worktree rows.
    expect(trimChats(rows)).toEqual([
      { kind: "section", section: "folders", expanded: true, count: 1 },
      { kind: "folder", folderId: 10 },
    ])
  })

  it("collapses only the root sub-group, leaving worktrees visible", () => {
    const wtConv = conv(2, 11)
    const rows = buildRows({
      pinned: [],
      pinnedExpanded: true,
      orderedFolderIds: [10],
      byFolder: new Map([
        [10, [conv(1, 10)]],
        [11, [wtConv]],
      ]),
      folderExpanded: {},
      folderTotalCounts: new Map(),
      foldersExpanded: true,
      chatConversations: [],
      chatsExpanded: true,
      containerChildren: new Map([[10, [11]]]),
      rootGroupCollapsed: new Set([10]),
    })
    expect(trimChats(rows)).toEqual([
      { kind: "section", section: "folders", expanded: true, count: 1 },
      { kind: "folder", folderId: 10 },
      { kind: "root-group", folderId: 10 }, // header shown, sessions hidden
      { kind: "folder", folderId: 11 },
      {
        kind: "conversation",
        conversation: wtConv,
        depth: 1,
        rootId: wtConv.id,
        bucketKey: "folder:11",
      },
    ])
  })

  it("emits an empty hint for an empty root sub-group / worktree", () => {
    const rows = buildRows({
      pinned: [],
      pinnedExpanded: true,
      orderedFolderIds: [10],
      byFolder: new Map(),
      folderExpanded: {},
      folderTotalCounts: new Map([
        [10, 4],
        [11, 0],
      ]),
      foldersExpanded: true,
      chatConversations: [],
      chatsExpanded: true,
      containerChildren: new Map([[10, [11]]]),
    })
    expect(trimChats(rows)).toEqual([
      { kind: "section", section: "folders", expanded: true, count: 1 },
      { kind: "folder", folderId: 10 },
      { kind: "root-group", folderId: 10 },
      { kind: "empty", folderId: 10, totalConversationCount: 4 },
      { kind: "folder", folderId: 11 },
      { kind: "empty", folderId: 11, totalConversationCount: 0 },
    ])
  })

  it("renders a repo with no worktree children flat at depth 0 (no root-group)", () => {
    const c = conv(1, 20)
    const rows = buildRows({
      pinned: [],
      pinnedExpanded: true,
      orderedFolderIds: [10, 20],
      byFolder: new Map([
        [11, [conv(9, 11)]],
        [20, [c]],
      ]),
      folderExpanded: {},
      folderTotalCounts: new Map(),
      foldersExpanded: true,
      chatConversations: [],
      chatsExpanded: true,
      // Only repo 10 is a container; repo 20 stays a plain flat folder.
      containerChildren: new Map([[10, [11]]]),
    })
    const trimmed = trimChats(rows)
    // Repo 20 (plain): header + its own session at depth 0 — no root-group.
    expect(trimmed).toContainEqual({ kind: "folder", folderId: 20 })
    expect(trimmed).toContainEqual({
      kind: "conversation",
      conversation: c,
      depth: 0,
      rootId: c.id,
      bucketKey: "folder:20",
    })
    expect(
      trimmed.some((r) => r.kind === "root-group" && r.folderId === 20)
    ).toBe(false)
  })
})

describe("mergeChildrenById", () => {
  it("keeps live events over the snapshot by id and adds new children", () => {
    const snapA = conv(100, 1, { status: "pending" })
    const snapB = conv(102, 1)
    const eventA = conv(100, 1, { status: "completed" }) // newer status, same id
    const eventC = conv(101, 1) // new child absent from the snapshot
    const merged = mergeChildrenById([snapA, snapB], [eventA, eventC])
    // Activity (updated_at) descending / newest-first (the factory derives
    // updated_at from id, so higher id == more recent activity)
    expect(merged.map((c) => c.id)).toEqual([102, 101, 100])
    // the live event wins over the snapshot for the shared id
    expect(merged.find((c) => c.id === 100)!.status).toBe("completed")
  })

  it("sorts children by updated_at activity, not created_at", () => {
    const createdNewer = conv(100, 1, {
      created_at: "2026-07-18T04:00:00.000Z",
      updated_at: "2026-07-18T01:00:00.000Z",
    })
    const activeNewer = conv(101, 1, {
      created_at: "2026-07-18T01:00:00.000Z",
      updated_at: "2026-07-18T03:00:00.000Z",
    })
    expect(
      mergeChildrenById([createdNewer, activeNewer], []).map((c) => c.id)
    ).toEqual([101, 100])
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

  it("orders chat roots by effective updated time", () => {
    // created_at opposite of updated_at so a created-first regression fails.
    const createdNewer = conv(2, 99, {
      kind: "chat",
      created_at: "2026-07-18T03:00:00.000Z",
      updated_at: "2026-07-18T01:00:00.000Z",
    })
    const activeNewer = conv(1, 99, {
      kind: "chat",
      created_at: "2026-07-18T01:00:00.000Z",
      updated_at: "2026-07-18T02:00:00.000Z",
    })

    // Non-optimistic: updated_at wins → activeNewer first.
    expect(
      selectChatConversationsWithReuse(
        [createdNewer, activeNewer],
        true,
        []
      ).map((c) => c.id)
    ).toEqual([1, 2])

    // Optimistic overlay then reverses the pair.
    const optimistic = new Map([
      [
        2,
        {
          token: "t2",
          baselineUpdatedAt: createdNewer.updated_at,
          effectiveAt: "2026-07-18T03:00:00.000Z",
        },
      ],
    ])
    expect(
      selectChatConversationsWithReuse(
        [createdNewer, activeNewer],
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

describe("selectRecentConversationsWithReuse", () => {
  const open = new Set([10])

  it("mixes folder and chat conversations, newest first, excluding pinned", () => {
    const folderConv = conv(1, 10)
    const chatConv = conv(2, 99, { kind: "chat" }) // higher id → later created_at
    const pinned = conv(3, 10, { pinned_at: new Date(5000).toISOString() })
    const out = selectRecentConversationsWithReuse(
      [folderConv, chatConv, pinned],
      true,
      "created",
      open,
      []
    )
    // Pinned lives in its own section; the other two interleave by recency.
    expect(out.map((c) => c.id)).toEqual([2, 1])
  })

  it("drops folder conversations whose folder is closed, but never chats", () => {
    const inOpen = conv(1, 10)
    const inClosed = conv(2, 77)
    const chatConv = conv(3, 99, { kind: "chat" })
    const out = selectRecentConversationsWithReuse(
      [inOpen, inClosed, chatConv],
      true,
      "created",
      open,
      []
    )
    // `list_all_conversations` returns rows for closed folders too; Recent must
    // apply the same reachability rule the Folders section does. Chat
    // conversations live in a hidden folder that is never in the open set.
    expect(out.map((c) => c.id)).toEqual([3, 1])
  })

  it("excludes completed conversations unless showCompleted", () => {
    const done = conv(1, 10, { status: "completed" })
    const active = conv(2, 10)
    expect(
      selectRecentConversationsWithReuse(
        [done, active],
        false,
        "created",
        open,
        []
      ).map((c) => c.id)
    ).toEqual([2])
    expect(
      selectRecentConversationsWithReuse(
        [done, active],
        true,
        "created",
        open,
        []
      ).map((c) => c.id)
    ).toEqual([2, 1])
  })

  it("sorts by the active sort mode so the order matches each card's label", () => {
    const older = conv(1, 10, { updated_at: new Date(9_000_000).toISOString() })
    const newer = conv(2, 10, { updated_at: new Date(1_000).toISOString() })
    // Created-at descending puts the higher id first; updated-at flips them.
    expect(
      selectRecentConversationsWithReuse(
        [older, newer],
        true,
        "created",
        open,
        []
      ).map((c) => c.id)
    ).toEqual([2, 1])
    expect(
      selectRecentConversationsWithReuse(
        [older, newer],
        true,
        "updated",
        open,
        []
      ).map((c) => c.id)
    ).toEqual([1, 2])
  })

  it("returns the prev array when membership is referentially unchanged", () => {
    const a = conv(1, 10)
    const first = selectRecentConversationsWithReuse(
      [a],
      true,
      "created",
      open,
      []
    )
    const second = selectRecentConversationsWithReuse(
      [a],
      true,
      "created",
      open,
      first
    )
    expect(second).toBe(first)
  })
})

describe("selectPinnedWithReuse", () => {
  it("omits unpinned conversations from pinned membership", () => {
    const pinnedOnly = conv(1, 10, {
      pinned_at: "2026-07-18T01:00:00.000Z",
      updated_at: "2026-07-18T01:00:00.000Z",
    })
    const unpinned = conv(2, 10, {
      updated_at: "2026-07-18T05:00:00.000Z",
    })
    const pinnedChat = conv(3, 99, {
      kind: "chat",
      pinned_at: "2026-07-18T02:00:00.000Z",
      updated_at: "2026-07-18T02:00:00.000Z",
    })
    expect(
      selectPinnedWithReuse([unpinned, pinnedOnly, pinnedChat], []).map(
        (row) => row.id
      )
    ).toEqual([3, 1])
    expect(
      selectPinnedWithReuse([unpinned, pinnedOnly, pinnedChat], []).every(
        (row) => row.pinned_at != null
      )
    ).toBe(true)
  })

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

  it("prefers the canonical row over a Recent duplicate above it", () => {
    // Recent listed first, so the duplicate is the EARLIER match — "locate the
    // active conversation" must still land on the folder row.
    const withRecentFirst: SidebarRow[] = [
      { kind: "section", section: "recent", expanded: true, count: 1 },
      {
        kind: "conversation",
        conversation: conv(1, 10),
        depth: 0,
        recent: true,
      },
      { kind: "section", section: "folders", expanded: true, count: 1 },
      { kind: "folder", folderId: 10 },
      { kind: "conversation", conversation: conv(1, 10), depth: 0 },
    ]
    expect(flatIndexOfConversation(withRecentFirst, 1, "claude_code")).toBe(4)
  })

  it("falls back to the Recent row when it is the only occurrence", () => {
    // The folder section is collapsed, so Recent is the one place the row
    // exists — better to scroll there than to give up.
    const recentOnly: SidebarRow[] = [
      { kind: "section", section: "folders", expanded: false, count: 1 },
      { kind: "section", section: "recent", expanded: true, count: 1 },
      {
        kind: "conversation",
        conversation: conv(1, 10),
        depth: 0,
        recent: true,
      },
    ]
    expect(flatIndexOfConversation(recentOnly, 1, "claude_code")).toBe(2)
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

    it("ends a folder's span at the next section header", () => {
      // The Chat / Recent rows below the Folders section belong to no folder:
      // without the reset they would inherit folder 10 and keep its sticky
      // header pinned over a list it has nothing to do with.
      const acrossSections: SidebarRow[] = [
        { kind: "section", section: "folders", expanded: true, count: 1 }, // 0
        { kind: "folder", folderId: 10 }, // 1
        { kind: "conversation", conversation: conv(1, 10), depth: 0 }, // 2
        { kind: "section", section: "recent", expanded: true, count: 1 }, // 3
        {
          kind: "conversation",
          conversation: conv(1, 10),
          depth: 0,
          recent: true,
        }, // 4
      ]
      expect(Array.from(buildOwnerHeaderIndex(acrossSections))).toEqual([
        -1, 1, 1, -1, -1,
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
