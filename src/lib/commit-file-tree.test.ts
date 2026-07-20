import { describe, expect, it } from "vitest"
import {
  buildCommitFileTree,
  collectExpandedDirectoryPaths,
  type CommitFileTreeDirNode,
} from "./commit-file-tree"
import type { GitLogFileChange } from "./types"

const change = (path: string): GitLogFileChange => ({
  path,
  status: "M",
  additions: 1,
  deletions: 0,
})

describe("buildCommitFileTree", () => {
  it("groups files into directories, sorting dirs before files", () => {
    const tree = buildCommitFileTree([
      change("src/lib/utils.ts"),
      change("README.md"),
      change("src/lib/api.ts"),
    ])

    expect(tree.map((n) => n.name)).toEqual(["src/lib", "README.md"])
    const [dir] = tree
    expect(dir.kind).toBe("dir")
    const dirNode = dir as CommitFileTreeDirNode
    expect(dirNode.fileCount).toBe(2)
    expect(dirNode.children.map((c) => c.name)).toEqual(["api.ts", "utils.ts"])
  })

  it("compresses single-child directory chains", () => {
    const [dir] = buildCommitFileTree([change("a/b/c/file.ts")])
    expect(dir.kind).toBe("dir")
    expect(dir.name).toBe("a/b/c")
  })

  it("ignores empty paths", () => {
    expect(buildCommitFileTree([change("")])).toEqual([])
  })
})

describe("collectExpandedDirectoryPaths", () => {
  it("returns every directory path in the tree", () => {
    const tree = buildCommitFileTree([
      change("src/lib/utils.ts"),
      change("src/app/page.tsx"),
    ])
    expect(collectExpandedDirectoryPaths(tree)).toEqual(
      new Set(["src", "src/lib", "src/app"])
    )
  })
})
