import { render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"

vi.mock("@/hooks/use-acp-agents", () => ({
  useAcpAgents: vi.fn(),
}))

import { DelegationAgentDefaultsPanel } from "./delegation-agent-defaults"
import { useAcpAgents } from "@/hooks/use-acp-agents"
import enMessages from "@/i18n/messages/en.json"
import type { AcpAgentInfo, AgentType } from "@/lib/types"

const mockUseAcpAgents = vi.mocked(useAcpAgents)

function agent(
  agentType: AgentType,
  enabled: boolean,
  name: string
): AcpAgentInfo {
  return {
    agent_type: agentType,
    skills_capable: true,
    registry_id: `${agentType}-registry`,
    registry_version: null,
    name,
    description: "",
    available: true,
    distribution_type: "system",
    custom_source: "manual",
    enabled,
    show_thinking: false,
    sort_order: 0,
    installed_version: null,
    env: {},
    config_json: null,
    config_file_path: null,
    opencode_auth_json: null,
    codex_auth_json: null,
    codex_config_toml: null,
    codex_model_catalog: null,
    codex_sandbox_settings: null,
    cline_secrets_json: null,
    hermes_config_yaml: null,
    grok_config_toml: null,
    grok_settings: null,
    cursor_cli_config_json: null,
    cursor_settings: null,
    model_provider_id: null,
    icon_url: null,
  }
}

beforeEach(() => {
  mockUseAcpAgents.mockReset()
})

describe("DelegationAgentDefaultsPanel", () => {
  it("omits disabled custom agents from the defaults target list", () => {
    mockUseAcpAgents.mockReturnValue({
      agents: [
        agent("custom:enabled-agent", true, "Enabled Agent"),
        agent("custom:disabled-agent", false, "Disabled Agent"),
      ],
      fresh: true,
      refresh: async () => {},
    })

    render(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <DelegationAgentDefaultsPanel value={{}} onChange={() => {}} />
      </NextIntlClientProvider>
    )

    expect(
      screen.getByRole("tab", { name: "Enabled Agent" })
    ).toBeInTheDocument()
    expect(
      screen.queryByRole("tab", { name: "Disabled Agent" })
    ).not.toBeInTheDocument()
  })
})
