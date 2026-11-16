import { render, screen } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

const viewport = vi.hoisted(() => ({ mobile: false }))
vi.mock("@/hooks/use-mobile", () => ({ useIsMobile: () => viewport.mobile }))
vi.mock("@/components/layout/status-bar-stats", () => ({
  StatusBarStats: () => <span data-testid="status-bar-stats" />,
}))
vi.mock("@/components/layout/status-bar-tasks", () => ({
  StatusBarTasks: () => null,
}))
vi.mock("@/components/layout/status-bar-alerts", () => ({
  StatusBarAlerts: () => null,
}))
vi.mock("@/components/layout/status-bar-mcp", () => ({
  StatusBarMcp: () => null,
}))
vi.mock("@/components/layout/status-bar-computer", () => ({
  StatusBarComputer: () => <span data-testid="status-bar-computer" />,
}))
vi.mock("@/components/layout/status-bar-update", () => ({
  StatusBarUpdate: () => null,
}))
vi.mock("@/components/layout/command-dropdown", () => ({
  CommandDropdown: () => null,
}))
vi.mock("@/components/layout/quick-actions-dropdown", () => ({
  QuickActionsDropdown: () => null,
}))
vi.mock("@/components/layout/status-bar-web-service", () => ({
  StatusBarWebService: () => <span data-testid="web-service" />,
}))

import { StatusBar } from "./status-bar"

beforeEach(() => {
  viewport.mobile = false
})

describe("StatusBar", () => {
  /** "Narrow" is a viewport width, and a desktop window can be shrunk below
   *  it: the web-service signal must not vanish with it. (The item itself
   *  renders nothing outside the local desktop.) */
  it.each([
    ["wide", false],
    ["narrow", true],
  ])("carries the web-service item in the %s layout", (_label, mobile) => {
    viewport.mobile = mobile
    render(<StatusBar />)
    expect(screen.getByTestId("web-service")).toBeInTheDocument()
    expect(screen.getByTestId("status-bar-stats")).toBeInTheDocument()
    expect(
      screen.queryByTestId("status-bar-session-model")
    ).not.toBeInTheDocument()
    if (mobile) {
      expect(
        screen.queryByTestId("status-bar-computer")
      ).not.toBeInTheDocument()
    } else {
      expect(screen.getByTestId("status-bar-computer")).toBeInTheDocument()
    }
  })
})
