import type { ReactNode } from "react"
import { render, screen } from "@testing-library/react"
import { describe, expect, it, vi } from "vitest"
import { useWorkbenchRoute } from "@/contexts/workbench-route-context"
import { DetachedShellProviders } from "./detached-shell"

vi.mock("@/contexts/alert-context", () => ({
  AlertProvider: ({ children }: { children: ReactNode }) => children,
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
    } finally {
      consoleError.mockRestore()
    }
  })
})
