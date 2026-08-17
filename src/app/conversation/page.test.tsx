import type { ReactNode } from "react"
import { render, screen, waitFor } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

const h = vi.hoisted(() => {
  const messages: Record<string, string> = {
    title: "Conversation",
    untitled: "Untitled",
    invalidParams: "Invalid conversation window parameters.",
    loading: "Loading conversation...",
    localDesktopOnly:
      "Conversation pop-out is only available in the local desktop app.",
    liveHandoffFailed: "Failed to take over the live session.",
  }

  return {
    searchParams: new URLSearchParams(
      "conversationId=42&folderId=7&agentType=codex&mode=web"
    ),
    translate: (key: string) => messages[key] ?? key,
    isDesktop: vi.fn(() => false),
    isLocalDesktop: vi.fn(() => false),
    getFolderConversation: vi.fn(),
    getFolder: vi.fn(),
    findConnection: vi.fn(),
    getPopoutOperation: vi.fn(),
    rebind: vi.fn(),
    claim: vi.fn(),
    setSuppress: vi.fn(),
    subscribe: vi.fn(),
    emit: vi.fn(),
    seedFolder: vi.fn(),
    seedSummary: vi.fn(),
    seedSessionTab: vi.fn(),
    surfaceProps: [] as Array<Record<string, unknown>>,
  }
})

vi.mock("next/navigation", () => ({
  useSearchParams: () => h.searchParams,
}))

vi.mock("next-intl", () => ({
  useTranslations: () => h.translate,
}))

vi.mock("@/lib/platform", () => ({
  isDesktop: () => h.isDesktop(),
  isLocalDesktop: () => h.isLocalDesktop(),
  subscribe: (...args: unknown[]) => h.subscribe(...args),
}))

vi.mock("@/lib/api", () => ({
  acpFindConnectionForConversation: (...args: unknown[]) =>
    h.findConnection(...args),
  getFolderConversation: (...args: unknown[]) =>
    h.getFolderConversation(...args),
  getFolder: (...args: unknown[]) => h.getFolder(...args),
  getConversationPopoutOperation: (...args: unknown[]) =>
    h.getPopoutOperation(...args),
  rebindConnectionOwnerWindow: (...args: unknown[]) => h.rebind(...args),
}))

vi.mock("@/lib/conversation-popout-acp-bridge", () => ({
  claimConnectionOwnership: (...args: unknown[]) => h.claim(...args),
  setSuppressFrontendDisconnect: (...args: unknown[]) => h.setSuppress(...args),
}))

vi.mock("@tauri-apps/api/event", () => ({
  emit: (...args: unknown[]) => h.emit(...args),
}))

vi.mock("@/components/layout/app-title-bar", () => ({
  AppTitleBar: ({ center }: { center?: ReactNode }) => <div>{center}</div>,
}))

vi.mock("@/components/ui/app-toaster", () => ({
  AppToaster: () => null,
}))

vi.mock("@/components/conversations/conversation-session-surface", () => ({
  ConversationSessionSurface: (props: Record<string, unknown>) => {
    h.surfaceProps.push(props)
    return <div data-testid="conversation-surface" />
  },
}))

vi.mock("@/contexts/remote-connection-context", () => ({
  RemoteConnectionGate: ({ children }: { children: ReactNode }) => children,
}))

vi.mock("./_components/detached-shell", () => ({
  DetachedShellProviders: ({ children }: { children: ReactNode }) => children,
  DetachedOpenTabKeysRegistrar: ({ contextKey }: { contextKey: string }) => (
    <div data-testid="open-tab-key" data-context-key={contextKey} />
  ),
  seedDetachedFolder: (...args: unknown[]) => h.seedFolder(...args),
  seedDetachedConversationSummary: (...args: unknown[]) =>
    h.seedSummary(...args),
  seedDetachedSessionTab: (...args: unknown[]) => h.seedSessionTab(...args),
}))

import * as conversationPage from "./page"

const { default: ConversationPage } = conversationPage

it("keeps the Next route module limited to its default export", () => {
  expect(conversationPage).not.toHaveProperty("ConversationPageInner")
})

describe("ConversationPageInner route bootstrap", () => {
  beforeEach(() => {
    vi.clearAllMocks()
    h.surfaceProps.length = 0
    h.searchParams = new URLSearchParams(
      "conversationId=42&folderId=7&agentType=codex&mode=web"
    )
    h.isDesktop.mockReturnValue(false)
    h.isLocalDesktop.mockReturnValue(false)
    h.getFolderConversation.mockResolvedValue({
      summary: {
        id: 42,
        folder_id: 7,
        agent_type: "codex",
        external_id: "session-42",
        title: "Web conversation",
      },
    })
    h.getFolder.mockResolvedValue({
      id: 7,
      path: "D:/repo",
      git_branch: "main",
    })
    h.findConnection.mockResolvedValue({
      connection_id: "existing-owner",
      event_seq: 4,
    })
    h.rebind.mockResolvedValue({
      ownershipGeneration: 3,
      operationId: "op-1",
      reboundCount: 1,
    })
    h.claim.mockResolvedValue({
      connectionId: "existing-owner",
      ownershipGeneration: 3,
    })
    h.getPopoutOperation.mockResolvedValue({ phase: "handoff_complete" })
    h.seedSessionTab.mockReturnValue("conv-7-codex-42")
    h.subscribe.mockResolvedValue(() => {})
    h.emit.mockResolvedValue(undefined)
  })

  it("seeds and activates web without any desktop handoff call", async () => {
    render(<ConversationPage />)

    await screen.findByTestId("conversation-surface")
    expect(h.getFolderConversation).toHaveBeenCalledTimes(1)
    expect(h.getFolderConversation).toHaveBeenCalledWith(42)
    expect(h.getFolder).toHaveBeenCalledTimes(1)
    expect(h.getFolder).toHaveBeenCalledWith(7)
    expect(h.seedFolder).toHaveBeenCalledTimes(1)
    expect(h.seedSummary).toHaveBeenCalledTimes(1)
    expect(h.seedSessionTab).toHaveBeenCalledTimes(1)
    expect(h.seedSessionTab).toHaveBeenCalledWith({
      folderId: 7,
      conversationId: 42,
      agentType: "codex",
      workingDir: "D:/repo",
      title: "Web conversation",
    })
    expect(screen.getByTestId("open-tab-key")).toHaveAttribute(
      "data-context-key",
      "conv-7-codex-42"
    )
    expect(h.surfaceProps.at(-1)).toMatchObject({
      tabId: "conv-7-codex-42",
      conversationId: 42,
      folderId: 7,
      agentType: "codex",
      workingDir: "D:/repo",
      isActive: true,
      ownerOperationId: null,
    })
    expect(h.findConnection).not.toHaveBeenCalled()
    expect(h.rebind).not.toHaveBeenCalled()
    expect(h.claim).not.toHaveBeenCalled()
    expect(h.setSuppress).not.toHaveBeenCalled()
    expect(h.subscribe).not.toHaveBeenCalled()
    expect(h.getPopoutOperation).not.toHaveBeenCalled()
    expect(h.emit).not.toHaveBeenCalled()
  })

  it.each([
    {
      name: "remote desktop mode=web",
      query: "conversationId=42&folderId=7&agentType=codex&mode=web",
      desktop: true,
      localDesktop: false,
    },
    {
      name: "pure web desktop-shaped URL",
      query: "conversationId=42&folderId=7&agentType=codex&operationId=op-1",
      desktop: false,
      localDesktop: false,
    },
    {
      name: "local desktop mode=web",
      query: "conversationId=42&folderId=7&agentType=codex&mode=web",
      desktop: true,
      localDesktop: true,
    },
  ])("rejects $name before metadata", async (runtime) => {
    h.searchParams = new URLSearchParams(runtime.query)
    h.isDesktop.mockReturnValue(runtime.desktop)
    h.isLocalDesktop.mockReturnValue(runtime.localDesktop)

    render(<ConversationPage />)

    expect(
      await screen.findByText(
        "Conversation pop-out is only available in the local desktop app."
      )
    ).toBeInTheDocument()
    expect(h.getFolderConversation).not.toHaveBeenCalled()
    expect(h.getFolder).not.toHaveBeenCalled()
    expect(h.findConnection).not.toHaveBeenCalled()
    expect(h.rebind).not.toHaveBeenCalled()
    expect(h.emit).not.toHaveBeenCalled()
  })

  it("preserves the local-desktop live rebind, ready, and commit-ack path", async () => {
    h.searchParams = new URLSearchParams(
      "conversationId=42&folderId=7&agentType=codex&operationId=op-1"
    )
    h.isDesktop.mockReturnValue(true)
    h.isLocalDesktop.mockReturnValue(true)
    h.subscribe.mockImplementation(
      async (_event: unknown, handler: (payload: unknown) => void) => {
        queueMicrotask(() => handler({ operationId: "op-1" }))
        return () => {}
      }
    )

    render(<ConversationPage />)

    await waitFor(() => expect(h.emit).toHaveBeenCalledTimes(1))
    expect(h.getFolderConversation).toHaveBeenCalledTimes(1)
    expect(h.getFolder).toHaveBeenCalledTimes(1)
    expect(h.seedFolder).toHaveBeenCalledTimes(1)
    expect(h.seedSummary).toHaveBeenCalledTimes(1)
    expect(h.seedSessionTab).toHaveBeenCalledTimes(1)
    expect(h.findConnection).toHaveBeenCalledTimes(1)
    expect(h.findConnection).toHaveBeenCalledWith(42, "session-42", "codex")
    expect(h.rebind).toHaveBeenCalledTimes(1)
    expect(h.rebind).toHaveBeenCalledWith({
      conversationId: 42,
      connectionId: "existing-owner",
      fromOwnerWindow: "main",
      toOwnerWindow: "conversation-42",
      operationId: "op-1",
    })
    expect(h.claim).toHaveBeenCalledTimes(1)
    expect(h.setSuppress).toHaveBeenCalledTimes(1)
    expect(h.setSuppress).toHaveBeenCalledWith(42, true)
    expect(h.subscribe).toHaveBeenCalledTimes(1)
    expect(h.emit).toHaveBeenCalledTimes(1)
    expect(h.emit).toHaveBeenCalledWith("conversation-window://ready", {
      conversationId: 42,
      operationId: "op-1",
      ownershipGeneration: 3,
      connectionId: "existing-owner",
    })
    await screen.findByTestId("conversation-surface")
    expect(h.surfaceProps.at(-1)).toMatchObject({
      isActive: true,
      ownerOperationId: "op-1",
    })
  })
})
