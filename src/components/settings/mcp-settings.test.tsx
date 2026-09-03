import { fireEvent, render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"

vi.mock("@/lib/api", () => ({
  mcpGetMarketplaceServerDetail: vi.fn(),
  mcpInstallFromMarketplace: vi.fn(),
  mcpListMarketplaces: vi.fn(),
  mcpRemoveServer: vi.fn(),
  mcpScanLocal: vi.fn(),
  mcpSearchMarketplace: vi.fn(),
  mcpUpsertLocalServer: vi.fn(),
}))

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn() },
}))

import { McpSettings } from "./mcp-settings"
import enMessages from "@/i18n/messages/en.json"
import { mcpListMarketplaces, mcpScanLocal } from "@/lib/api"

beforeEach(() => {
  vi.clearAllMocks()
  vi.mocked(mcpListMarketplaces).mockResolvedValue([])
  vi.mocked(mcpScanLocal).mockResolvedValue({
    servers: [
      {
        id: "ctx7",
        spec: { type: "stdio", command: "npx", args: ["-y", "ctx7"] },
        apps: ["codex"],
      },
    ],
    warnings: [
      {
        app: "antigravity",
        message: "invalid JSON at C:\\agents\\mcp.json",
      },
    ],
  })
})

describe("McpSettings", () => {
  it("shows degraded scan warnings and disables save and create", async () => {
    render(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <McpSettings />
      </NextIntlClientProvider>
    )

    expect(
      await screen.findByText(
        /Could not read the MCP config for Google Antigravity.*invalid JSON at C:\\agents\\mcp\.json/
      )
    ).toBeInTheDocument()
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled()

    fireEvent.click(screen.getByRole("button", { name: "New MCP" }))

    expect(screen.getByRole("button", { name: "Create" })).toBeDisabled()
  })
})
