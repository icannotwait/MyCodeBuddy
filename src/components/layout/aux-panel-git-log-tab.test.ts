import { describe, expect, it } from "vitest"

import {
  canResetFromSelection,
  HEAD_BRANCH_FILTER,
  isHeadFilter,
  isLiveBranchSelection,
} from "./aux-panel-git-log-tab"
import type { GitBranchList } from "@/lib/types"

const branchList: GitBranchList = {
  local: ["main", "feature/x"],
  remote: ["origin/main"],
  worktree_branches: [],
}

describe("isHeadFilter", () => {
  it("only recognizes the literal HEAD sentinel", () => {
    expect(isHeadFilter(HEAD_BRANCH_FILTER)).toBe(true)
    expect(isHeadFilter("HEAD")).toBe(true)
  })

  it("does not mistake a ref that merely mentions HEAD", () => {
    // git rejects `HEAD` as a local branch name and git_list_all_branches drops
    // remote refs containing HEAD, so nothing in the list can collide — but a
    // near-miss must never be treated as the dynamic view.
    expect(isHeadFilter("origin/HEAD")).toBe(false)
    expect(isHeadFilter("head")).toBe(false)
    expect(isHeadFilter("HEADs")).toBe(false)
    expect(isHeadFilter(null)).toBe(false)
  })
})

describe("isLiveBranchSelection", () => {
  it("keeps the HEAD sentinel even though it is not in the branch list", () => {
    // The dead-branch cleanup in refreshBranches would otherwise clear the
    // dynamic filter on the very first branch refresh.
    expect(isLiveBranchSelection(HEAD_BRANCH_FILTER, branchList)).toBe(true)
  })

  it("keeps the all-branches view", () => {
    expect(isLiveBranchSelection(null, branchList)).toBe(true)
  })

  it("keeps a branch that still exists locally or on a remote", () => {
    expect(isLiveBranchSelection("main", branchList)).toBe(true)
    expect(isLiveBranchSelection("feature/x", branchList)).toBe(true)
    expect(isLiveBranchSelection("origin/main", branchList)).toBe(true)
  })

  it("drops a branch that no longer exists", () => {
    expect(isLiveBranchSelection("feature/deleted", branchList)).toBe(false)
    // Bare name of a remote-only ref is not itself a branch here.
    expect(isLiveBranchSelection("origin/gone", branchList)).toBe(false)
  })
})

describe("canResetFromSelection", () => {
  it("allows reset from the all-branches view", () => {
    expect(canResetFromSelection("main", null)).toBe(true)
  })

  it("allows reset from the HEAD view", () => {
    // The HEAD view IS the current branch, so resetting it is consistent.
    expect(canResetFromSelection("main", HEAD_BRANCH_FILTER)).toBe(true)
  })

  it("allows reset while viewing the current branch by name", () => {
    expect(canResetFromSelection("main", "main")).toBe(true)
  })

  it("blocks reset while viewing a different branch", () => {
    expect(canResetFromSelection("main", "feature/x")).toBe(false)
    expect(canResetFromSelection("main", "origin/main")).toBe(false)
  })

  it("blocks reset with no current branch (detached HEAD)", () => {
    expect(canResetFromSelection(null, null)).toBe(false)
    expect(canResetFromSelection(null, HEAD_BRANCH_FILTER)).toBe(false)
  })
})
