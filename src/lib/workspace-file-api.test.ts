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

import { getFileTree, searchWorkspaceFiles } from "@/lib/api"
import {
  cancelWorkspaceFileSearch as cancelWorkspaceFileSearchTauri,
  getFolderConversation as getFolderConversationTauri,
  getFileTree as getFileTreeTauri,
  searchWorkspaceFiles as searchWorkspaceFilesTauri,
} from "@/lib/tauri"

const request = {
  consumerId: "picker",
  generation: 2,
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

    await searchWorkspaceFiles("/repo", "foo", { limit: 50, request })
    expect(mocks.call).toHaveBeenLastCalledWith(
      "search_workspace_files",
      {
        path: "/repo",
        query: "foo",
        limit: 50,
        request,
      },
      { timeoutMs: 330_000 }
    )
  })

  it("sends explicit defaults for legacy callers", async () => {
    await getFileTree("/repo")
    expect(mocks.call).toHaveBeenLastCalledWith("get_file_tree", {
      path: "/repo",
      maxDepth: null,
      includeIgnored: false,
    })

    await searchWorkspaceFiles("/repo", "foo")
    expect(mocks.call).toHaveBeenLastCalledWith(
      "search_workspace_files",
      {
        path: "/repo",
        query: "foo",
        request: { consumerId: expect.any(String), generation: 0 },
      },
      { timeoutMs: 330_000 }
    )
  })

  it("keeps direct Tauri wrappers in payload parity", async () => {
    await getFolderConversationTauri(42)
    expect(mocks.invoke).toHaveBeenLastCalledWith("get_folder_conversation", {
      conversationId: 42,
      historyUserTurnLimit: 20,
    })

    await getFileTreeTauri("/repo", 2, true)
    expect(mocks.invoke).toHaveBeenLastCalledWith("get_file_tree", {
      path: "/repo",
      maxDepth: 2,
      includeIgnored: true,
    })

    await searchWorkspaceFilesTauri("/repo", "foo", { limit: 50, request })
    expect(mocks.invoke).toHaveBeenLastCalledWith("search_workspace_files", {
      path: "/repo",
      query: "foo",
      limit: 50,
      request,
    })

    await cancelWorkspaceFileSearchTauri(request)
    expect(mocks.invoke).toHaveBeenLastCalledWith(
      "cancel_workspace_file_search",
      { request }
    )
  })

  it("does not mix direct Tauri index and user-turn windows", async () => {
    await getFolderConversationTauri(42, { tailTurns: 50 })
    expect(mocks.invoke).toHaveBeenLastCalledWith("get_folder_conversation", {
      conversationId: 42,
      tailTurns: 50,
    })

    await getFolderConversationTauri(42, { fromIndex: 120 })
    expect(mocks.invoke).toHaveBeenLastCalledWith("get_folder_conversation", {
      conversationId: 42,
      fromIndex: 120,
    })
  })
})
