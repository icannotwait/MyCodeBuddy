import { beforeEach, describe, expect, it, vi } from "vitest"
import { resetTabStore, useTabStore } from "@/stores/tab-store"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"
import {
  openDelegatedChildSession,
  resolveDelegatedChildOpenTarget,
} from "@/lib/open-delegated-child-session"

const childSummary = {
  id: 99,
  folder_id: 7,
  title: "Child title",
  title_locked: false,
  auto_title_finalized: false,
  agent_type: "codex" as const,
  status: "active",
  awaiting_reply_token: null,
  kind: "delegate" as const,
  model: null,
  git_branch: null,
  external_id: null,
  message_count: 0,
  child_count: 0,
  created_at: "",
  updated_at: "",
  pinned_at: null,
}

describe("resolveDelegatedChildOpenTarget", () => {
  beforeEach(() => {
    resetTabStore()
    useAppWorkspaceStore.setState({
      conversations: [childSummary],
    } as never)
  })

  it("prefers workspace list folder_id", () => {
    expect(
      resolveDelegatedChildOpenTarget({
        childConversationId: 99,
        agentType: "codex",
      })
    ).toEqual({
      folderId: 7,
      conversationId: 99,
      agentType: "codex",
      title: "Child title",
    })
  })

  it("falls back to active tab folderId when child is absent from list", () => {
    useAppWorkspaceStore.setState({ conversations: [] } as never)
    useTabStore.setState({
      rawTabs: [
        {
          id: "p",
          kind: "conversation",
          folderId: 3,
          conversationId: 10,
          agentType: "claude_code",
          title: "Parent",
          isPinned: false,
          activationSeq: 1,
        },
      ],
      activeTabId: "p",
      tabsHydrated: true,
    })
    expect(
      resolveDelegatedChildOpenTarget({
        childConversationId: 99,
        agentType: "codex",
        title: "Kickoff",
      })
    ).toEqual({
      folderId: 3,
      conversationId: 99,
      agentType: "codex",
      title: "Kickoff",
    })
  })

  it("returns null when id, agentType, or folderId missing", () => {
    useAppWorkspaceStore.setState({ conversations: [] } as never)
    useTabStore.setState({ rawTabs: [], activeTabId: null })
    expect(
      resolveDelegatedChildOpenTarget({
        childConversationId: 99,
        agentType: "codex",
      })
    ).toBeNull()
    expect(
      resolveDelegatedChildOpenTarget({
        childConversationId: null,
        agentType: "codex",
      })
    ).toBeNull()
    expect(
      resolveDelegatedChildOpenTarget({
        childConversationId: 99,
        agentType: null,
      })
    ).toBeNull()
  })

  it("returns null when conversationId is non-positive", () => {
    expect(
      resolveDelegatedChildOpenTarget({
        childConversationId: 0,
        agentType: "codex",
      })
    ).toBeNull()
  })

  it("prefers summary title over input title", () => {
    expect(
      resolveDelegatedChildOpenTarget({
        childConversationId: 99,
        agentType: "codex",
        title: "Kickoff",
      })
    ).toEqual({
      folderId: 7,
      conversationId: 99,
      agentType: "codex",
      title: "Child title",
    })
  })
})

describe("openDelegatedChildSession", () => {
  beforeEach(() => {
    resetTabStore()
  })

  it("no-ops when resolve fails", async () => {
    useAppWorkspaceStore.setState({ conversations: [] } as never)
    useTabStore.setState({ rawTabs: [], activeTabId: null })
    await expect(
      openDelegatedChildSession({
        childConversationId: 1,
        agentType: null,
      })
    ).resolves.toBe(false)
  })

  it("calls openTab with resolved target", async () => {
    const openTab = vi.fn(async () => true)
    useTabStore.setState({ openTab } as never)
    useAppWorkspaceStore.setState({
      conversations: [childSummary],
    } as never)

    await expect(
      openDelegatedChildSession({
        childConversationId: 99,
        agentType: "codex",
      })
    ).resolves.toBe(true)

    expect(openTab).toHaveBeenCalledTimes(1)
    expect(openTab).toHaveBeenCalledWith(7, 99, "codex", false, "Child title")
  })
})
