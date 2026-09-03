import { render, screen } from "@testing-library/react"
import { describe, expect, it, vi } from "vitest"
import { StatusBar } from "./status-bar"

const mobileState = vi.hoisted(() => ({ value: false }))

vi.mock("@/hooks/use-mobile", () => ({
  useIsMobile: () => mobileState.value,
}))
vi.mock("./status-bar-stats", () => ({
  StatusBarStats: () => <span data-testid="status-bar-stats" />,
}))
vi.mock("./status-bar-tasks", () => ({
  StatusBarTasks: () => <span data-testid="status-bar-tasks" />,
}))
vi.mock("./status-bar-alerts", () => ({
  StatusBarAlerts: () => <span data-testid="status-bar-alerts" />,
}))
vi.mock("./status-bar-update", () => ({
  StatusBarUpdate: () => <span data-testid="status-bar-update" />,
}))
vi.mock("./command-dropdown", () => ({
  CommandDropdown: () => <span data-testid="command-dropdown" />,
}))
vi.mock("./quick-actions-dropdown", () => ({
  QuickActionsDropdown: () => <span data-testid="quick-actions-dropdown" />,
}))

describe("StatusBar", () => {
  it.each([false, true])(
    "does not render session model chip on mobile=%s",
    (mobile) => {
      mobileState.value = mobile
      render(<StatusBar />)

      expect(screen.getByTestId("status-bar-stats")).toBeInTheDocument()
      expect(
        screen.queryByTestId("status-bar-session-model")
      ).not.toBeInTheDocument()
    }
  )
})
