import { beforeEach, describe, expect, it, vi } from "vitest"

const mocks = vi.hoisted(() => ({
  call: vi.fn(),
  invoke: vi.fn(),
}))

vi.mock("@/lib/transport", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/lib/transport")>()),
  getTransport: () => ({ call: mocks.call }),
}))

vi.mock("@tauri-apps/api/core", () => ({
  invoke: mocks.invoke,
}))

import {
  cancelWorkspaceFileSearch,
  getFileTree,
  searchWorkspaceFiles,
} from "@/lib/api"
import {
  cancelWorkspaceFileSearch as cancelWorkspaceFileSearchTauri,
  getFileTree as getFileTreeTauri,
  searchWorkspaceFiles as searchWorkspaceFilesTauri,
} from "@/lib/tauri"

const identity = {
  searchSessionId: "session-1",
  requestId: "request-1",
}

describe("workspace file API payloads", () => {
  beforeEach(() => {
    mocks.call.mockReset().mockResolvedValue([])
    mocks.invoke.mockReset().mockResolvedValue([])
  })

  it("serializes tree mode, search identity, and cancellation", async () => {
    await getFileTree("/repo", 2, true)
    expect(mocks.call).toHaveBeenLastCalledWith("get_file_tree", {
      path: "/repo",
      maxDepth: 2,
      includeIgnored: true,
    })

    await searchWorkspaceFiles("/repo", "foo", 50, identity)
    expect(mocks.call).toHaveBeenLastCalledWith("search_workspace_files", {
      path: "/repo",
      query: "foo",
      limit: 50,
      ...identity,
    })

    await cancelWorkspaceFileSearch(identity)
    expect(mocks.call).toHaveBeenLastCalledWith(
      "cancel_workspace_file_search",
      identity
    )
  })

  it("sends explicit defaults for legacy callers", async () => {
    await getFileTree("/repo")
    expect(mocks.call).toHaveBeenLastCalledWith("get_file_tree", {
      path: "/repo",
      maxDepth: null,
      includeIgnored: false,
    })

    await searchWorkspaceFiles("/repo")
    expect(mocks.call).toHaveBeenLastCalledWith("search_workspace_files", {
      path: "/repo",
      query: "",
      limit: 50,
      searchSessionId: null,
      requestId: null,
    })
  })

  it("keeps direct Tauri wrappers in payload parity", async () => {
    await getFileTreeTauri("/repo", 2, true)
    expect(mocks.invoke).toHaveBeenLastCalledWith("get_file_tree", {
      path: "/repo",
      maxDepth: 2,
      includeIgnored: true,
    })

    await searchWorkspaceFilesTauri("/repo", "foo", 50, identity)
    expect(mocks.invoke).toHaveBeenLastCalledWith("search_workspace_files", {
      path: "/repo",
      query: "foo",
      limit: 50,
      ...identity,
    })

    await cancelWorkspaceFileSearchTauri(identity)
    expect(mocks.invoke).toHaveBeenLastCalledWith(
      "cancel_workspace_file_search",
      identity
    )
  })
})
