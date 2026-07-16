import { act, cleanup, render, waitFor } from "@testing-library/react"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import type { DbConversationSummary } from "@/lib/types"
import {
  resetAppWorkspaceStore,
  useAppWorkspaceStore,
} from "@/stores/app-workspace-store"
import { resetTabStore, useTabStore } from "@/stores/tab-store"

const clearAwaitingReply = vi.fn()
const onTransportReconnect = vi.fn()
const routeMock = { isConversations: true }
const workspaceMock = { filesMaximized: false }

vi.mock("@/lib/api", () => ({
  clearAwaitingReply: (...args: unknown[]) => clearAwaitingReply(...args),
}))

vi.mock("@/lib/platform", () => ({
  onTransportReconnect: (...args: unknown[]) => onTransportReconnect(...args),
}))

vi.mock("@/contexts/workbench-route-context", () => ({
  useWorkbenchRoute: () => routeMock,
}))

vi.mock("@/contexts/workspace-context", () => ({
  useWorkspaceView: () => workspaceMock,
}))

import { ConversationAwaitingReplyClearer } from "./conversation-awaiting-reply-clearer"

function makeSummary(
  overrides: Partial<DbConversationSummary> & { id: number }
): DbConversationSummary {
  return {
    folder_id: 1,
    title: null,
    title_locked: false,
    agent_type: "claude_code",
    status: "pending_review",
    awaiting_reply_token: null,
    kind: "regular",
    model: null,
    git_branch: null,
    external_id: null,
    message_count: 0,
    child_count: 0,
    created_at: "2026-01-01T00:00:00.000Z",
    updated_at: "2026-07-16T01:00:00.000Z",
    pinned_at: null,
    parent_id: null,
    parent_tool_use_id: null,
    delegation_call_id: null,
    ...overrides,
  }
}

function makeTab(
  conversationId: number,
  overrides: Partial<{ id: string; title: string }> = {}
) {
  const id = overrides.id ?? `conv-1-claude_code-${conversationId}`
  return {
    id,
    kind: "conversation" as const,
    folderId: 1,
    conversationId,
    agentType: "claude_code" as const,
    title: overrides.title ?? `Conversation ${conversationId}`,
    isPinned: false,
  }
}

let visibilityState: DocumentVisibilityState = "visible"
let documentHasFocus: ReturnType<typeof vi.spyOn>

function setDocumentVisibility(state: DocumentVisibilityState) {
  visibilityState = state
  document.dispatchEvent(new Event("visibilitychange"))
}

function nullTokenPatch(id: number, updatedAt = "2026-07-16T02:00:00.000Z") {
  return {
    id,
    status: "pending_review",
    awaiting_reply_token: null,
    updated_at: updatedAt,
  }
}

function seedActiveConversation(opts: {
  id: number
  awaiting_reply_token: string
}) {
  const tab = makeTab(opts.id)
  useTabStore.setState({
    tabsHydrated: true,
    isTileMode: false,
    activeTabId: tab.id,
    rawTabs: [tab],
    tabs: [tab],
  })
  useAppWorkspaceStore.getState().applyConversationUpsert(
    makeSummary({
      id: opts.id,
      awaiting_reply_token: opts.awaiting_reply_token,
    })
  )
}

function seedTiledConversations(opts: {
  activeId: number
  inactiveId: number
  inactiveToken: string
}) {
  const activeTab = makeTab(opts.activeId)
  const inactiveTab = makeTab(opts.inactiveId)
  useTabStore.setState({
    tabsHydrated: true,
    isTileMode: true,
    activeTabId: activeTab.id,
    rawTabs: [activeTab, inactiveTab],
    tabs: [activeTab, inactiveTab],
  })
  useAppWorkspaceStore.getState().applyConversationUpsert(
    makeSummary({
      id: opts.activeId,
      awaiting_reply_token: null,
    })
  )
  useAppWorkspaceStore.getState().applyConversationUpsert(
    makeSummary({
      id: opts.inactiveId,
      awaiting_reply_token: opts.inactiveToken,
    })
  )
}

describe("ConversationAwaitingReplyClearer", () => {
  beforeEach(() => {
    resetAppWorkspaceStore()
    resetTabStore()
    clearAwaitingReply.mockReset()
    onTransportReconnect.mockReset()
    onTransportReconnect.mockReturnValue(() => {})
    routeMock.isConversations = true
    workspaceMock.filesMaximized = false
    visibilityState = "visible"
    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      get: () => visibilityState,
    })
    documentHasFocus = vi.spyOn(document, "hasFocus").mockReturnValue(true)
    clearAwaitingReply.mockImplementation(async (id: number) =>
      nullTokenPatch(id)
    )
  })

  afterEach(() => {
    cleanup()
    documentHasFocus.mockRestore()
  })

  it("clears the active token once while the conversation is genuinely visible", async () => {
    seedActiveConversation({ id: 7, awaiting_reply_token: "generation-7" })
    documentHasFocus.mockReturnValue(true)
    render(<ConversationAwaitingReplyClearer />)

    await waitFor(() =>
      expect(clearAwaitingReply).toHaveBeenCalledWith(7, "generation-7")
    )
    expect(clearAwaitingReply).toHaveBeenCalledTimes(1)
    expect(
      useAppWorkspaceStore.getState().conversations[0].awaiting_reply_token
    ).toBeNull()
  })

  it.each([
    [
      "automations route",
      {
        isConversations: false,
        filesMaximized: false,
        visible: true,
        focused: true,
      },
    ],
    [
      "maximized files",
      {
        isConversations: true,
        filesMaximized: true,
        visible: true,
        focused: true,
      },
    ],
    [
      "hidden document",
      {
        isConversations: true,
        filesMaximized: false,
        visible: false,
        focused: true,
      },
    ],
    [
      "unfocused document",
      {
        isConversations: true,
        filesMaximized: false,
        visible: true,
        focused: false,
      },
    ],
  ])("does not clear while %s", async (_label, state) => {
    seedActiveConversation({ id: 8, awaiting_reply_token: "generation-8" })
    routeMock.isConversations = state.isConversations
    workspaceMock.filesMaximized = state.filesMaximized
    setDocumentVisibility(state.visible ? "visible" : "hidden")
    documentHasFocus.mockReturnValue(state.focused)
    render(<ConversationAwaitingReplyClearer />)
    await Promise.resolve()
    expect(clearAwaitingReply).not.toHaveBeenCalled()
  })

  it("does not clear a token owned by an inactive tile", async () => {
    seedTiledConversations({
      activeId: 8,
      inactiveId: 9,
      inactiveToken: "generation-9",
    })
    documentHasFocus.mockReturnValue(true)
    render(<ConversationAwaitingReplyClearer />)
    await Promise.resolve()
    expect(clearAwaitingReply).not.toHaveBeenCalled()
  })

  it("acknowledges a newer token with its own CAS after a stale clear loses", async () => {
    seedActiveConversation({ id: 9, awaiting_reply_token: "generation-a" })
    clearAwaitingReply
      .mockResolvedValueOnce({
        id: 9,
        status: "pending_review",
        awaiting_reply_token: "generation-b",
        updated_at: "2026-07-16T02:00:00.000Z",
      })
      .mockResolvedValueOnce({
        id: 9,
        status: "pending_review",
        awaiting_reply_token: null,
        updated_at: "2026-07-16T02:00:00.000Z",
      })
    render(<ConversationAwaitingReplyClearer />)
    await waitFor(() => {
      expect(clearAwaitingReply).toHaveBeenNthCalledWith(1, 9, "generation-a")
      expect(clearAwaitingReply).toHaveBeenNthCalledWith(2, 9, "generation-b")
    })
    expect(
      useAppWorkspaceStore.getState().conversations[0].awaiting_reply_token
    ).toBeNull()
  })

  it("clears after a focus transition makes the view qualifying", async () => {
    seedActiveConversation({ id: 10, awaiting_reply_token: "generation-10" })
    documentHasFocus.mockReturnValue(false)
    render(<ConversationAwaitingReplyClearer />)
    await Promise.resolve()
    expect(clearAwaitingReply).not.toHaveBeenCalled()

    documentHasFocus.mockReturnValue(true)
    act(() => {
      window.dispatchEvent(new Event("focus"))
    })

    await waitFor(() =>
      expect(clearAwaitingReply).toHaveBeenCalledWith(10, "generation-10")
    )
    expect(clearAwaitingReply).toHaveBeenCalledTimes(1)
  })

  it("retries a failed clear only after transport reconnect", async () => {
    seedActiveConversation({ id: 11, awaiting_reply_token: "generation-11" })
    let reconnectCb: (() => void) | null = null
    onTransportReconnect.mockImplementation((cb: () => void) => {
      reconnectCb = cb
      return () => {}
    })
    clearAwaitingReply
      .mockRejectedValueOnce(new Error("network"))
      .mockResolvedValueOnce(nullTokenPatch(11))

    const warn = vi.spyOn(console, "warn").mockImplementation(() => {})
    render(<ConversationAwaitingReplyClearer />)

    await waitFor(() => expect(clearAwaitingReply).toHaveBeenCalledTimes(1))
    expect(
      useAppWorkspaceStore.getState().conversations[0].awaiting_reply_token
    ).toBe("generation-11")

    // Still qualifying; no timer loop — wait a tick without reconnect.
    await act(async () => {
      await Promise.resolve()
    })
    expect(clearAwaitingReply).toHaveBeenCalledTimes(1)

    act(() => {
      reconnectCb?.()
    })

    await waitFor(() => expect(clearAwaitingReply).toHaveBeenCalledTimes(2))
    expect(clearAwaitingReply).toHaveBeenLastCalledWith(11, "generation-11")
    expect(
      useAppWorkspaceStore.getState().conversations[0].awaiting_reply_token
    ).toBeNull()
    warn.mockRestore()
  })

  it("retries clear after reconnect while the same key is still in-flight", async () => {
    seedActiveConversation({ id: 12, awaiting_reply_token: "generation-12" })
    let reconnectCb: (() => void) | null = null
    onTransportReconnect.mockImplementation((cb: () => void) => {
      reconnectCb = cb
      return () => {}
    })

    let rejectFirst!: (error: Error) => void
    const firstPending = new Promise<never>((_resolve, reject) => {
      rejectFirst = reject
    })
    clearAwaitingReply
      .mockImplementationOnce(() => firstPending)
      .mockResolvedValueOnce(nullTokenPatch(12))

    const warn = vi.spyOn(console, "warn").mockImplementation(() => {})
    render(<ConversationAwaitingReplyClearer />)

    await waitFor(() => expect(clearAwaitingReply).toHaveBeenCalledTimes(1))
    expect(clearAwaitingReply).toHaveBeenCalledWith(12, "generation-12")

    // Reconnect while first clear still pending — no concurrent duplicate.
    act(() => {
      reconnectCb?.()
    })
    await act(async () => {
      await Promise.resolve()
    })
    expect(clearAwaitingReply).toHaveBeenCalledTimes(1)

    // After first rejects and key is released, exactly one second clear.
    await act(async () => {
      rejectFirst(new Error("network"))
      await Promise.resolve()
    })

    await waitFor(() => expect(clearAwaitingReply).toHaveBeenCalledTimes(2))
    expect(clearAwaitingReply).toHaveBeenNthCalledWith(2, 12, "generation-12")
    await waitFor(() =>
      expect(
        useAppWorkspaceStore.getState().conversations[0].awaiting_reply_token
      ).toBeNull()
    )

    // No third call without another signal.
    await act(async () => {
      await Promise.resolve()
    })
    expect(clearAwaitingReply).toHaveBeenCalledTimes(2)
    expect(warn).toHaveBeenCalled()
    warn.mockRestore()
  })
})
