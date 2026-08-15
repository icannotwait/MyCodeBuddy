import { beforeEach, describe, expect, it, vi } from "vitest"

const mocks = vi.hoisted(() => ({
  call: vi.fn(),
  invoke: vi.fn(),
}))

vi.mock("@/lib/transport", () => ({
  getTransport: () => ({ call: mocks.call }),
}))

vi.mock("@tauri-apps/api/core", () => ({
  invoke: mocks.invoke,
}))

import { gitPush, gitPushInfo } from "@/lib/api"
import {
  gitPush as gitPushTauri,
  gitPushInfo as gitPushInfoTauri,
} from "@/lib/tauri"

describe("Git branch payloads", () => {
  beforeEach(() => {
    mocks.call.mockReset().mockResolvedValue({})
    mocks.invoke.mockReset().mockResolvedValue({})
  })

  it("keeps an explicit push branch in transport and direct Tauri payloads", async () => {
    const credentials = { username: "alice", password: "secret" }

    await gitPushInfo("/repo", "feature")
    expect(mocks.call).toHaveBeenLastCalledWith("git_push_info", {
      path: "/repo",
      branch: "feature",
    })

    await gitPush("/repo", "origin", credentials, 42, "feature")
    expect(mocks.call).toHaveBeenLastCalledWith("git_push", {
      path: "/repo",
      remote: "origin",
      branch: "feature",
      credentials,
      folderId: 42,
    })

    await gitPushInfoTauri("/repo", "feature")
    expect(mocks.invoke).toHaveBeenLastCalledWith("git_push_info", {
      path: "/repo",
      branch: "feature",
    })

    await gitPushTauri("/repo", "origin", credentials, 42, "feature")
    expect(mocks.invoke).toHaveBeenLastCalledWith("git_push", {
      path: "/repo",
      remote: "origin",
      branch: "feature",
      credentials,
      folderId: 42,
    })
  })
})
