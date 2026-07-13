import { fireEvent, render, screen, waitFor } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"

vi.mock("@/lib/api", () => ({
  getSystemTerminalSettings: vi.fn(),
  getAvailableTerminalShells: vi.fn(),
  updateSystemTerminalSettings: vi.fn(),
  probeTerminalShellPath: vi.fn(),
  getSystemRenderingSettings: vi.fn(),
  updateSystemRenderingSettings: vi.fn(),
}))

vi.mock("@/lib/platform", () => ({
  isDesktop: () => false,
}))

vi.mock("@/lib/transport", () => ({
  getActiveRemoteConnectionId: () => null,
}))

vi.mock("@/hooks/use-platform", () => ({
  usePlatform: () => ({ isWindows: true, platform: "windows" }),
}))

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), message: vi.fn() },
}))

// Keep GeneralSettings focused: child sections hit their own APIs.
vi.mock("@/components/settings/delegation-settings", () => ({
  DelegationSettingsSection: () => null,
}))
vi.mock("@/components/settings/session-feedback-settings", () => ({
  SessionFeedbackSettingsSection: () => null,
}))
vi.mock("@/components/settings/ask-question-settings", () => ({
  AskQuestionSettingsSection: () => null,
}))
vi.mock("@/components/settings/session-info-settings", () => ({
  SessionInfoSettingsSection: () => null,
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

beforeEach(() => {
  mockGetSettings.mockReset()
  mockGetShells.mockReset()
  mockUpdateSettings.mockReset()
})

describe("GeneralSettings terminal shell", () => {
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
