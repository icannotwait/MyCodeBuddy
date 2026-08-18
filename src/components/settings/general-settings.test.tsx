import { fireEvent, render, screen, waitFor } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"

vi.mock("@/lib/api", () => ({
  getSystemTerminalSettings: vi.fn(async () => ({ default_shell: null })),
  getAvailableTerminalShells: vi.fn(async () => ({
    resolved_shell: "/bin/zsh",
    effective_shell: "/bin/zsh",
    options: [
      {
        id: "system",
        label_key: "terminalSystemDefault",
        value: null,
        exists: true,
        accepts_custom_path: false,
      },
      {
        id: "custom",
        label_key: "terminalShellCustom",
        value: null,
        exists: true,
        accepts_custom_path: true,
      },
    ],
  })),
  getSystemRenderingSettings: vi.fn(async () => ({
    disable_hardware_acceleration: false,
  })),
  updateSystemRenderingSettings: vi.fn(async (v: unknown) => v),
  updateSystemTerminalSettings: vi.fn(async (v: unknown) => v),
  probeTerminalShellPath: vi.fn(async () => true),
  getDelegationSettings: vi.fn(async () => ({
    enabled: false,
    depth_limit: 1,
    completed_cache_max_mb: 512,
    agent_defaults: {},
  })),
  setDelegationSettings: vi.fn(async (v: unknown) => v),
  getDelegationProfileCatalog: vi.fn(async () => ({ profiles: [] })),
  setDelegationBundle: vi.fn(async (v: unknown) => v),
  acpListAgents: vi.fn(async () => []),
  getFeedbackSettings: vi.fn(async () => ({ enabled: false })),
  setFeedbackSettings: vi.fn(async (v: unknown) => v),
  getQuestionSettings: vi.fn(async () => ({ enabled: true })),
  setQuestionSettings: vi.fn(async (v: unknown) => v),
  getSessionInfoSettings: vi.fn(async () => ({ enabled: true })),
  setSessionInfoSettings: vi.fn(async (v: unknown) => v),
  getChatAuthoringSettings: vi.fn(async () => ({
    automations_enabled: false,
    work_tasks_enabled: false,
  })),
  setChatAuthoringSettings: vi.fn(async (v: unknown) => v),
}))

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), message: vi.fn() },
}))
vi.mock("@/lib/platform", () => ({ isDesktop: () => true }))
vi.mock("@/lib/transport", () => ({ getActiveRemoteConnectionId: () => null }))
vi.mock("@/hooks/use-platform", () => ({
  usePlatform: () => ({
    platform: "windows",
    isMac: false,
    isWindows: true,
    isLinux: false,
  }),
}))
vi.mock("@/lib/updater", () => ({ relaunchApp: vi.fn() }))
vi.mock("@/hooks/use-feedback-enabled", () => ({
  primeFeedbackEnabled: vi.fn(),
}))

// Fork-only section; the upstream "mounts every section" suite does not assert it.
vi.mock("@/components/settings/conversation-experience-settings", () => ({
  ConversationExperienceSettingsSection: () => null,
}))

import { GeneralSettings } from "./general-settings"
import enMessages from "@/i18n/messages/en.json"
import {
  getAvailableTerminalShells,
  getSystemTerminalSettings,
  updateSystemTerminalSettings,
} from "@/lib/api"
import type { AvailableTerminalShells } from "@/lib/types"

const mockGetSettings = vi.mocked(getSystemTerminalSettings)
const mockGetShells = vi.mocked(getAvailableTerminalShells)
const mockUpdateSettings = vi.mocked(updateSystemTerminalSettings)

const baseOptions: AvailableTerminalShells["options"] = [
  {
    id: "system",
    label_key: "terminalSystemDefault",
    value: null,
    exists: true,
    accepts_custom_path: false,
  },
  {
    id: "pwsh.exe",
    label_key: "terminalPowerShell7",
    value: "pwsh.exe",
    exists: true,
    accepts_custom_path: false,
  },
  {
    id: "cmd.exe",
    label_key: "terminalCmd",
    value: "cmd.exe",
    exists: true,
    accepts_custom_path: false,
  },
]

function renderWithIntl() {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <GeneralSettings />
    </NextIntlClientProvider>
  )
}

describe("GeneralSettings terminal shell", () => {
  beforeEach(() => {
    mockGetSettings.mockClear()
    mockGetShells.mockClear()
    mockUpdateSettings.mockClear()
  })

  it("shows the selected effective shell and expanded scope", async () => {
    mockGetSettings.mockResolvedValue({
      default_shell: "pwsh.exe",
    })
    mockGetShells.mockResolvedValue({
      options: [
        {
          id: "pwsh.exe",
          label_key: "terminalPowerShell7",
          value: "pwsh.exe",
          exists: true,
          accepts_custom_path: false,
        },
      ],
      effective_shell: "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
    })

    renderWithIntl()

    expect(
      await screen.findByText(/C:\\Program Files\\PowerShell\\7\\pwsh.exe/)
    ).toBeInTheDocument()
    expect(
      screen.getByText(/new ACP agent tool execution/i)
    ).toBeInTheDocument()
  })

  it("persists CMD and renders the refreshed effective shell", async () => {
    mockGetSettings.mockResolvedValue({
      default_shell: "pwsh.exe",
    })
    mockGetShells
      .mockResolvedValueOnce({
        options: baseOptions,
        effective_shell: "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
      })
      .mockResolvedValueOnce({
        options: baseOptions,
        effective_shell: "C:\\Windows\\System32\\cmd.exe",
      })
    mockUpdateSettings.mockResolvedValue({ default_shell: "cmd.exe" })

    renderWithIntl()

    expect(
      await screen.findByText(/C:\\Program Files\\PowerShell\\7\\pwsh.exe/)
    ).toBeInTheDocument()

    fireEvent.click(screen.getByRole("combobox"))
    fireEvent.click(
      await screen.findByRole("option", { name: /Command Prompt \(cmd\)/i })
    )

    await waitFor(() => {
      expect(mockUpdateSettings).toHaveBeenCalledWith({
        default_shell: "cmd.exe",
      })
    })

    expect(
      await screen.findByText(/C:\\Windows\\System32\\cmd\.exe/)
    ).toBeInTheDocument()
  })
})

/**
 * The page is a stack of sections rendered through the shared
 * `SettingsSection` / `SettingCard` / `SettingRow` grammar, so what is worth
 * pinning is the wiring that grammar carries: every row's label resolves to the
 * control it names (a `SettingRow` with the `htmlFor` left off still looks
 * right and silently loses the association), and each section actually mounts.
 */
describe("GeneralSettings", () => {
  beforeEach(() => {
    mockGetSettings.mockResolvedValue({ default_shell: null })
    mockGetShells.mockResolvedValue({
      resolved_shell: "/bin/zsh",
      effective_shell: "/bin/zsh",
      options: baseOptions,
    })
  })
  it("mounts every section and wires each row's label to its control", async () => {
    render(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <GeneralSettings />
      </NextIntlClientProvider>
    )

    // Terminal section: the heading itself names the picker.
    const shell = await screen.findByLabelText("Default Terminal")
    expect(shell).toBeInTheDocument()
    expect(
      await screen.findByText(
        /Effective shell for new terminals and ACP connections: \/bin\/zsh/
      )
    ).toBeInTheDocument()

    // Rendering section: checkbox → Switch.
    const hwAccel = screen.getByLabelText("Disable hardware acceleration")
    expect(hwAccel).toHaveAttribute("role", "switch")
    expect(hwAccel).toHaveAttribute("data-state", "unchecked")
    fireEvent.click(hwAccel)
    await waitFor(() =>
      expect(hwAccel).toHaveAttribute("data-state", "checked")
    )

    // Every child section mounted. A section that is one option is titled by
    // that option, so these double as the labels asserted above.
    for (const heading of [
      "Default Terminal",
      "Disable hardware acceleration",
      "Notification sounds",
      "Multi-Agent Collaboration",
      "In-conversation tools",
    ]) {
      expect(screen.getByRole("heading", { name: heading })).toBeInTheDocument()
    }

    // Sibling toggles keep their label association through SettingRow.
    expect(screen.getByLabelText("Enable delegation")).toBeInTheDocument()
    expect(screen.getByLabelText("Live Feedback")).toBeInTheDocument()
    expect(screen.getByLabelText("Ask user question")).toBeInTheDocument()
    expect(screen.getByLabelText("Get session info")).toBeInTheDocument()
    expect(screen.getByLabelText("Create automations")).toBeInTheDocument()
    expect(screen.getByLabelText("Create to-do tasks")).toBeInTheDocument()
  })
})
