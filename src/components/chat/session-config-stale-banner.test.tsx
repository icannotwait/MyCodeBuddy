import { fireEvent, render, screen, waitFor } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

import { SessionConfigStaleBanner } from "./session-config-stale-banner"

const h = vi.hoisted(() => ({
  configStale: true,
  configStaleKind: "terminal_shell" as
    | "agent_config"
    | "model_provider"
    | "terminal_shell"
    | null,
  configStaleDismissed: false,
  isViewer: false,
  isDelegationChild: false,
  status: "connected" as string | null,
  reapplyConfig: vi.fn(async () => true),
  dismissConfigStale: vi.fn(),
}))

vi.mock("next-intl", () => ({
  useTranslations: () => (key: string) => key,
}))

vi.mock("@/hooks/use-connection", () => ({
  useConnection: () => ({
    configStale: h.configStale,
    configStaleKind: h.configStaleKind,
    configStaleDismissed: h.configStaleDismissed,
    isViewer: h.isViewer,
    isDelegationChild: h.isDelegationChild,
    status: h.status,
    reapplyConfig: h.reapplyConfig,
    dismissConfigStale: h.dismissConfigStale,
  }),
}))

vi.mock("sonner", () => ({
  toast: {
    success: vi.fn(),
    error: vi.fn(),
  },
}))

describe("SessionConfigStaleBanner", () => {
  beforeEach(() => {
    h.configStale = true
    h.configStaleKind = "terminal_shell"
    h.configStaleDismissed = false
    h.isViewer = false
    h.isDelegationChild = false
    h.status = "connected"
    h.reapplyConfig.mockClear()
    h.reapplyConfig.mockResolvedValue(true)
    h.dismissConfigStale.mockClear()
  })

  it("renders nothing when config is not stale", () => {
    h.configStale = false
    const { container } = render(<SessionConfigStaleBanner contextKey="tab-1" />)
    expect(container.firstChild).toBeNull()
  })

  it("shows terminalShellTitle for terminal_shell kind and reconnects once", async () => {
    render(<SessionConfigStaleBanner contextKey="tab-1" />)

    expect(screen.getByText("terminalShellTitle")).toBeInTheDocument()
    expect(screen.getByText("description")).toBeInTheDocument()

    fireEvent.click(screen.getByRole("button", { name: /reconnect/i }))

    await waitFor(() => {
      expect(h.reapplyConfig).toHaveBeenCalledTimes(1)
    })
  })

  it("shows modelProviderTitle for model_provider kind", () => {
    h.configStaleKind = "model_provider"
    render(<SessionConfigStaleBanner contextKey="tab-1" />)
    expect(screen.getByText("modelProviderTitle")).toBeInTheDocument()
  })

  it("shows agentConfigTitle for agent_config kind", () => {
    h.configStaleKind = "agent_config"
    render(<SessionConfigStaleBanner contextKey="tab-1" />)
    expect(screen.getByText("agentConfigTitle")).toBeInTheDocument()
  })
})
