import { describe, expect, it } from "vitest"
import type { DbConversationSummary } from "@/lib/types"
import type {
  ConversationRow,
  SidebarBucketKey,
  SidebarRow,
} from "./sidebar-conversation-grouping"
import {
  buildSidebarRootOrderSnapshot,
  detectSidebarActivityReorder,
  selectSidebarAnchor,
  sidebarAnchorScrollDelta,
  sidebarFlipDeltaY,
  type SidebarMeasuredRow,
  type SidebarRootOrderSnapshot,
} from "./sidebar-reorder-animation"

type RejectedScenario =
  | "pin-transfer"
  | "filter-membership"
  | "folder-collapse"
  | "subtree-expansion"
  | "unrelated-structure"
  | "downward-move"

function root(
  id: number,
  bucketKey: SidebarBucketKey = "folder:10",
  folderId = 10,
  rootId = id,
  depth = 0
): ConversationRow {
  return {
    kind: "conversation",
    conversation: {
      id,
      agent_type: "claude_code",
      folder_id: folderId,
    } as DbConversationSummary,
    depth,
    rootId,
    bucketKey,
  }
}

function rowsForFolder(roots: ConversationRow[]): SidebarRow[] {
  return [
    { kind: "section", section: "folders", expanded: true, count: 1 },
    { kind: "folder", folderId: 10 },
    ...roots,
  ]
}

function scenarioFixture(scenario: RejectedScenario): {
  before: SidebarRootOrderSnapshot
  after: SidebarRootOrderSnapshot
  activityId: number
} {
  const beforeRows = rowsForFolder([root(1), root(2), root(3)])
  let afterRows: SidebarRow[] = beforeRows
  switch (scenario) {
    case "pin-transfer":
      afterRows = rowsForFolder([root(3, "pinned"), root(1), root(2)])
      break
    case "filter-membership":
      afterRows = rowsForFolder([root(1), root(2)])
      break
    case "folder-collapse":
      afterRows = rowsForFolder([])
      break
    case "subtree-expansion":
      afterRows = rowsForFolder([
        root(3),
        root(30, "folder:10", 10, 3, 1),
        root(1),
        root(2),
      ])
      break
    case "unrelated-structure":
      afterRows = [
        { kind: "section", section: "folders", expanded: true, count: 1 },
        { kind: "empty", folderId: 99, totalConversationCount: 0 },
        { kind: "folder", folderId: 10 },
        root(3),
        root(1),
        root(2),
      ]
      break
    case "downward-move":
      return {
        before: buildSidebarRootOrderSnapshot(
          rowsForFolder([root(3), root(1), root(2)])
        ),
        after: buildSidebarRootOrderSnapshot(beforeRows),
        activityId: 3,
      }
  }
  return {
    before: buildSidebarRootOrderSnapshot(beforeRows),
    after: buildSidebarRootOrderSnapshot(afterRows),
    activityId: 3,
  }
}

describe("buildSidebarRootOrderSnapshot", () => {
  it("stores row-provided bucketKey, never derives from folder_id", () => {
    const snap = buildSidebarRootOrderSnapshot(
      rowsForFolder([root(4, "folder:10", 11)])
    )
    expect(snap.bucketByRoot.get(4)).toBe("folder:10")
  })

  it("puts owned loading placeholders in the root block, not structural keys", () => {
    const rows: SidebarRow[] = [
      { kind: "section", section: "folders", expanded: true, count: 1 },
      { kind: "folder", folderId: 10 },
      root(1),
      {
        kind: "subsession-loading",
        parentId: 1,
        depth: 1,
        rootId: 1,
        bucketKey: "folder:10",
      },
    ]
    const snap = buildSidebarRootOrderSnapshot(rows)
    expect(snap.structuralRowKeys).toEqual(["section-folders", "folder-10"])
    expect(snap.structuralRowKeys).not.toContain("subloading-1")
    expect(snap.blockRowKeysByRoot.get(1)).toEqual([
      "conv-claude_code-1",
      "subloading-1",
    ])
    expect(snap.rootsByBucket.get("folder:10")).toEqual([1])
  })

  it("preserves unowned structural keys and root block order", () => {
    const snap = buildSidebarRootOrderSnapshot(
      rowsForFolder([root(1), root(2), root(3)])
    )
    expect(snap.structuralRowKeys).toEqual(["section-folders", "folder-10"])
    expect(snap.rootsByBucket.get("folder:10")).toEqual([1, 2, 3])
    expect(snap.blockRowKeysByRoot.get(2)).toEqual(["conv-claude_code-2"])
    expect(snap.bucketByRoot.get(2)).toBe("folder:10")
  })
})

describe("detectSidebarActivityReorder", () => {
  it("accepts only an upward same-bucket root permutation", () => {
    const before = buildSidebarRootOrderSnapshot(
      rowsForFolder([root(1), root(2), root(3)])
    )
    const after = buildSidebarRootOrderSnapshot(
      rowsForFolder([root(3), root(1), root(2)])
    )
    expect(detectSidebarActivityReorder(before, after, 3)).toEqual({
      conversationId: 3,
      bucketKey: "folder:10",
      previousIndex: 2,
      nextIndex: 0,
    })
  })

  it.each([
    "pin-transfer",
    "filter-membership",
    "folder-collapse",
    "subtree-expansion",
    "unrelated-structure",
    "downward-move",
  ] as const)("rejects %s", (scenario) => {
    const { before, after, activityId } = scenarioFixture(scenario)
    expect(detectSidebarActivityReorder(before, after, activityId)).toBeNull()
  })

  it("rejects malformed snapshots whose root lists carry internal duplicates", () => {
    // Builder invariant is one depth-0 entry per root. A broken multiset like
    // [1,1,2] vs [1,2,2] must not pass sameNumberSet membership as equivalent.
    const before: SidebarRootOrderSnapshot = {
      structuralRowKeys: ["section-folders", "folder-10"],
      rootsByBucket: new Map([["folder:10", [1, 1, 2]]]),
      blockRowKeysByRoot: new Map([
        [1, ["conv-claude_code-1"]],
        [2, ["conv-claude_code-2"]],
      ]),
      bucketByRoot: new Map([
        [1, "folder:10"],
        [2, "folder:10"],
      ]),
    }
    const after: SidebarRootOrderSnapshot = {
      structuralRowKeys: ["section-folders", "folder-10"],
      rootsByBucket: new Map([["folder:10", [2, 1, 2]]]),
      blockRowKeysByRoot: new Map([
        [1, ["conv-claude_code-1"]],
        [2, ["conv-claude_code-2"]],
      ]),
      bucketByRoot: new Map([
        [1, "folder:10"],
        [2, "folder:10"],
      ]),
    }
    expect(detectSidebarActivityReorder(before, after, 2)).toBeNull()
  })
})

describe("selectSidebarAnchor", () => {
  it("chooses the first fully visible surviving row outside the promoted block", () => {
    const before = new Map<string, SidebarMeasuredRow>([
      ["partial", { key: "partial", rootId: 1, top: 90, bottom: 120 }],
      ["promoted", { key: "promoted", rootId: 3, top: 120, bottom: 152 }],
      ["stable", { key: "stable", rootId: 1, top: 152, bottom: 184 }],
    ])
    expect(
      selectSidebarAnchor(
        before,
        new Set(["partial", "promoted", "stable"]),
        100,
        300,
        3
      )?.key
    ).toBe("stable")
  })

  it("returns null when no eligible survivor remains", () => {
    const before = new Map<string, SidebarMeasuredRow>([
      ["promoted", { key: "promoted", rootId: 3, top: 120, bottom: 152 }],
      ["gone", { key: "gone", rootId: 1, top: 152, bottom: 184 }],
    ])
    expect(
      selectSidebarAnchor(before, new Set(["promoted"]), 100, 300, 3)
    ).toBeNull()
  })

  it("accepts rows that touch exact viewport edges", () => {
    const before = new Map<string, SidebarMeasuredRow>([
      ["edge", { key: "edge", rootId: 1, top: 100, bottom: 300 }],
    ])
    expect(
      selectSidebarAnchor(before, new Set(["edge"]), 100, 300, 9)?.key
    ).toBe("edge")
  })

  it("selects by ascending top, not map insertion order", () => {
    const before = new Map<string, SidebarMeasuredRow>([
      ["lower", { key: "lower", rootId: 2, top: 200, bottom: 230 }],
      ["upper", { key: "upper", rootId: 1, top: 120, bottom: 150 }],
    ])
    expect(
      selectSidebarAnchor(before, new Set(["lower", "upper"]), 100, 300, 9)?.key
    ).toBe("upper")
  })
})

describe("sidebar delta helpers", () => {
  it("uses opposite signs for scroll correction and FLIP", () => {
    expect(sidebarAnchorScrollDelta(152, 184)).toBe(32)
    expect(sidebarFlipDeltaY(152, 184)).toBe(-32)
  })

  it("returns zero deltas when tops are unchanged", () => {
    expect(sidebarAnchorScrollDelta(100, 100)).toBe(0)
    expect(sidebarFlipDeltaY(100, 100)).toBe(0)
  })
})
