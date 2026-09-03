import type { ReactNode } from "react"
import { render, screen } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"
import { useWorkbenchRoute } from "@/contexts/workbench-route-context"
import type { DbConversationSummary, FolderDetail } from "@/lib/types"
import {
  resetAppWorkspaceStore,
  useAppWorkspaceStore,
} from "@/stores/app-workspace-store"
import {
  DetachedShellProviders,
  seedDetachedConversationSummary,
  seedDetachedFolder,
} from "./detached-shell"

const summary: DbConversationSummary = {
  id: 42,
  folder_id: 7,
  title: "Cold pop-out",
  title_locked: false,
  auto_title_finalized: false,
  agent_type: "codex",
  status: "pending_review",
  awaiting_reply_token: null,
  kind: "regular",
  model: null,
  git_branch: null,
  external_id: "session-42",
  message_count: 1,
  child_count: 0,
  created_at: "2026-07-23T00:00:00.000Z",
  updated_at: "2026-07-23T00:00:00.000Z",
  pinned_at: null,
  parent_id: null,
  parent_tool_use_id: null,
  delegation_call_id: null,
}

const folder: FolderDetail = {
  id: 7,
  name: "repo",
  path: "/repo",
  git_branch: "feature/popout",
  default_agent_type: null,
  last_agent_type: null,
  last_opened_at: "2026-07-23T00:00:00.000Z",
  sort_order: 0,
  color: "inherit",
  parent_id: null,
  kind: "regular",
  alias: null,
  group_id: null,
}

vi.mock("@/contexts/alert-context", () => ({
  AlertProvider: ({ children }: { children: ReactNode }) => children,
}))

vi.mock("@/contexts/app-workspace-context", () => ({
  AppWorkspaceProvider: ({ children }: { children: ReactNode }) => (
    <div data-testid="app-workspace-provider">{children}</div>
  ),
}))

vi.mock("@/contexts/task-context", () => ({
  TaskProvider: ({ children }: { children: ReactNode }) => children,
}))

vi.mock("@/contexts/acp-connections-context", () => ({
  AcpConnectionsProvider: ({ children }: { children: ReactNode }) => children,
  useAcpActions: () => ({ registerOpenTabKeys: () => {} }),
}))

vi.mock("@/contexts/conversation-runtime-context", () => ({
  ConversationRuntimeProvider: ({ children }: { children: ReactNode }) =>
    children,
}))

vi.mock("@/contexts/delegation-context", () => ({
  DelegationProvider: ({ children }: { children: ReactNode }) => children,
}))

vi.mock("@/contexts/git-credential-context", () => ({
  GitCredentialProvider: ({ children }: { children: ReactNode }) => children,
}))

vi.mock("@/contexts/workspace-context", () => ({
  WorkspaceProvider: ({ children }: { children: ReactNode }) => children,
}))

function RouteProbe() {
  const { routeId, isConversations } = useWorkbenchRoute()
  return (
    <output data-testid="route-context">
      {routeId}:{String(isConversations)}
    </output>
  )
}

beforeEach(() => {
  resetAppWorkspaceStore()
})

describe("detached workspace state seeding", () => {
  it("seeds the persisted summary used by the durable ACP gate", () => {
    seedDetachedConversationSummary(summary)

    expect(useAppWorkspaceStore.getState().conversations).toEqual([summary])
  })

  it("seeds a non-null folder branch as an immediate fallback", () => {
    seedDetachedFolder(folder)

    expect(useAppWorkspaceStore.getState().getBranch(folder.id)).toBe(
      "feature/popout"
    )
  })

  it("does not replace a polled branch with a null folder fallback", () => {
    useAppWorkspaceStore.getState().setBranch(folder.id, "feature/live-head")

    seedDetachedFolder({ ...folder, git_branch: null })

    expect(useAppWorkspaceStore.getState().getBranch(folder.id)).toBe(
      "feature/live-head"
    )
  })
})

describe("DetachedShellProviders", () => {
  it("provides the workbench route context to detached session children", () => {
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {})
    try {
      render(
        <DetachedShellProviders>
          <RouteProbe />
        </DetachedShellProviders>
      )
      expect(screen.getByTestId("route-context")).toHaveTextContent(
        "conversations:true"
      )
      const route = screen.getByTestId("route-context")
      expect(screen.getByTestId("app-workspace-provider")).toContainElement(
        route
      )
    } finally {
      consoleError.mockRestore()
    }
  })
})
