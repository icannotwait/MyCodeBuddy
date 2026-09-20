import {
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"

vi.mock("@/lib/api", () => ({
  getSystemTerminalSettings: vi.fn(async () => ({
    default_shell: null,
    colorize_command_output: false,
  })),
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
  getBrowserToolsSettings: vi.fn(async () => ({ enabled: false })),
  setSessionInfoSettings: vi.fn(async (v: unknown) => v),
  getChatAuthoringSettings: vi.fn(async () => ({
    automations_enabled: false,
    work_tasks_enabled: false,
  })),
  setChatAuthoringSettings: vi.fn(async (v: unknown) => v),
  getSystemCloseBehaviorSettings: vi.fn(async () => ({
    behavior: "ask" as const,
    tray_available: true,
  })),
  updateSystemCloseBehaviorSettings: vi.fn(async (behavior: string) => ({
    behavior,
    tray_available: true,
  })),
}))

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), message: vi.fn() },
}))
vi.mock("@/lib/platform", () => ({
  isDesktop: () => true,
  // The delegation and agent-tools sections subscribe to their settings-change
  // broadcasts so a form left open converges instead of reverting a write made
  // elsewhere (the status-bar codeg-mcp popover).
  subscribe: () => Promise.resolve(() => {}),
}))
vi.mock("@/lib/transport", () => ({
  getActiveRemoteConnectionId: () => null,
  // Read by the desktop-notification section to decide whether permission is
  // the browser's to grant or the OS's; `false` puts it on the browser branch,
  // which is the one with visible controls to assert on.
  isDesktop: () => false,
  getShellTransport: () => ({ call: vi.fn() }),
}))
// The rendering section is gated on the host webview having an env knob to
// flip, so the platform has to be steerable per test. `vi.hoisted` because the
// `vi.mock` factory is lifted above every plain `const` in this file.
const platform = vi.hoisted(() => ({ current: "windows" as PlatformType }))
vi.mock("@/hooks/use-platform", () => ({
  usePlatform: () => ({
    platform: platform.current,
    isMac: platform.current === "macos",
    isWindows: platform.current === "windows",
    isLinux: platform.current === "linux",
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
// Task 15 still owns this file (conflict markers). Stub the headings/labels
// the "mounts every section" suite asserts so this suite can run in parallel.
vi.mock("@/components/settings/delegation-settings", () => ({
  DelegationSettingsSection: () => (
    <section>
      <h2>Multi-Agent Collaboration</h2>
      <label htmlFor="enable-delegation">Enable delegation</label>
      <input id="enable-delegation" />
    </section>
  ),
}))
// Value-imports conflicted `@/lib/types`; stub the labels this suite asserts.
vi.mock("@/components/settings/agent-tools-settings", () => ({
  AgentToolsSettingsSection: () => (
    <section>
      <h2>In-conversation tools</h2>
      <label htmlFor="live-feedback">Live Feedback</label>
      <input id="live-feedback" />
      <label htmlFor="ask-user">Ask user question</label>
      <input id="ask-user" />
      <label htmlFor="session-info">Get session info</label>
      <input id="session-info" />
      <label htmlFor="create-auto">Create automations</label>
      <input id="create-auto" />
      <label htmlFor="create-todo">Create to-do tasks</label>
      <input id="create-todo" />
    </section>
  ),
}))

import { GeneralSettings } from "./general-settings"
import type { PlatformType } from "@/hooks/use-platform"
import {
  getAvailableTerminalShells,
  getSystemTerminalSettings,
  updateSystemTerminalSettings,
} from "@/lib/api"

// Task 18 still owns the locale files (conflict markers). Fixture is the
// union of the namespaces this page actually renders.
const enMessages = {
  GeneralSettings: {
    loading: "Loading...",
    sectionTitle: "General",
    sectionDescription:
      "Centralized preferences for the default terminal, rendering acceleration, notifications, and multi-agent delegation.",
    terminalTitle: "Default Terminal",
    terminalDescription:
      "Choose the shell used for new terminal tabs and new ACP agent tool execution. Running terminals and ACP connections keep their current shell; reconnect an ACP session to apply changes.",
    terminalSystemDefault: "System default",
    terminalPowerShell7: "PowerShell 7 (pwsh)",
    terminalWindowsPowerShell: "Windows PowerShell",
    terminalCmd: "Command Prompt (cmd)",
    terminalSaveFailed: "Failed to save terminal settings: {message}",
    terminalShellCustom: "Custom path",
    terminalShellCustomPath: "Shell path",
    terminalShellCustomPlaceholder: "/usr/local/bin/fish",
    terminalShellCustomSave: "Save",
    terminalShellCustomHint:
      "Provide an absolute path or a name resolvable on PATH.",
    terminalShellNotInstalled: "not installed",
    terminalShellNotFoundWarning: "This path doesn't exist on this host.",
    terminalCurrentShell:
      "Effective shell for new terminals and ACP connections: {path}",
    renderingDescription:
      "Disable hardware acceleration if the app shows a black screen or rendering glitches (common on certain AMD GPUs or Intel integrated GPUs on Windows, and with the proprietary NVIDIA driver on Linux). Only takes effect on the Windows and Linux desktop builds.",
    disableHardwareAcceleration: "Disable hardware acceleration",
    renderingSaveFailed: "Failed to save rendering settings: {message}",
    restartRequired: "Saved. Restart the app for the change to take effect.",
    restartNow: "Restart now",
    restartFailed: "Failed to restart: {message}",
    loadFailed: "Load failed: {message}",
    conversationExperienceTitle: "Conversation experience",
    conversationExperienceDescription:
      "Configure automatic titles, document translation, and reference search behavior.",
    conversationExperienceLoadFailed:
      "Failed to load conversation experience settings.",
    conversationExperienceRetry: "Retry",
    autoTitleSaveFailed: "Failed to save automatic title settings: {message}",
    autoTitleLoading: "Loading automatic title settings...",
    translateProviderDisclosure:
      "Document text is sent to the selected agent/provider for translation. Codeg hides internal sessions from its own lists but does not delete the agent CLI’s storage.",
    referenceSearchLimit: "Reference result limit",
    referenceSearchLimitHint:
      "Maximum cached and searched results per resource source (10-500).",
    referenceSearchLimitSave: "Save reference limit",
    referenceSearchLimitSaveFailed: "Failed to save reference limit: {message}",
    autoTitleSection: "Automatic titles",
    autoTitleApiUrl: "API Base URL",
    autoTitleApiKey: "API Key",
    autoTitleApiKeyPlaceholder: "Enter API key",
    autoTitleApiKeySetPlaceholder: "Key is set — leave blank to keep",
    autoTitleApiKeyClearedPlaceholder: "Key will be cleared on save",
    autoTitleClearKey: "Clear key",
    autoTitleModel: "Model",
    autoTitleStatusEnabled: "Automatic titles: On",
    autoTitleStatusIncomplete:
      "Automatic titles: Off (configuration incomplete)",
    autoTitleStatusBarrier:
      "Configuration incomplete — re-save or re-enter key",
    autoTitleHttpDisclosure:
      "The first user message and first usable assistant reply are sent to the configured endpoint for title generation. Use a trusted provider; prefer HTTPS.",
    autoTitleSave: "Save title API settings",
    documentTranslateAgent: "Document translation agent",
    documentTranslateOff: "Off",
    documentTranslateUnavailable: "{agent} (Unavailable)",
    documentTranslateSaveFailed:
      "Failed to save document translation agent: {message}",
    terminalAgentShellHint:
      "On System default, agent command lines run through the platform shell (/bin/sh, or cmd on Windows). Picking a specific shell applies it to agents too — they emit POSIX syntax, which fish, nushell, and Windows PowerShell 5.1 (no && operator) may reject.",
    colorizeCommandOutput: "Colorize command output",
    colorizeCommandOutputDescription:
      "Force ANSI color out of the commands agents run, so their output renders in color in the transcript instead of as plain text. Takes effect on new sessions.",
  },
  DesktopNotificationSettings: {
    title: "Desktop notifications",
    description:
      "Raise an OS notification when an agent needs you. Configured per device — the same setting on another browser or machine is separate.",
    permissionTitle: "Permission",
    permissionGranted: "Allowed",
    permissionDenied: "Blocked",
    permissionDefault: "Not requested",
    permissionUnsupported: "Unavailable",
    permissionManagedByOs: "Managed by the system",
    permissionGrantedHint: "This browser will show notifications from Codeg.",
    permissionDeniedHint:
      "This browser is blocking notifications from Codeg. Only the browser's own site settings can undo that — a page cannot ask again.",
    permissionDefaultHint:
      "This browser has not been asked yet. Notifications stay silent until you allow them.",
    permissionUnsupportedHint:
      "Notifications need a secure context. Reach this server over HTTPS or on localhost.",
    permissionManagedByOsHint:
      "The desktop app posts through the system notification centre, which does not report back whether Codeg is allowed. Send a test to find out.",
    identityTitle: "Delivered as",
    identityHint:
      "The system files these notifications under this app — its own switches are the ones that apply.",
    identityDegradedHint:
      "Codeg's own identifier could not be claimed, so the system posts these as {bundleId} and that app's switches apply instead. Installing Codeg to your Applications folder restores its own identity.",
    requestPermission: "Allow notifications",
    permissionGrantedToast: "Notifications allowed.",
    permissionDeniedToast:
      "Notifications were blocked. Re-enable them in your browser's site settings.",
    openSystemSettings: "Open system settings",
    openSystemSettingsFailed:
      "Could not open the system notification settings.",
    sendTest: "Send a test",
    testTitle: "Codeg",
    testBody: "Desktop notifications are working.",
    testSent: "Test notification sent.",
    testFailed: "The test notification could not be sent.",
    whenTitle: "Notify when",
    whenHint:
      "A window on a second monitor is visible but not focused — pick “Window is not focused” if notifications never seem to arrive.",
    whenAlways: "Always",
    whenUnfocused: "Window is not focused",
    whenHidden: "Window is not visible",
    hideBody: "Hide notification contents",
    hideBodyHint:
      "Show a generic line instead of agent output. A notification centre keeps its payload outside the app, after the conversation is closed. The title still names the folder, so you can tell which window to go back to.",
    eventsTitle: "Events",
    eventsHint:
      "Turn off any event that should not interrupt you. Repeats of the same event within a few seconds collapse into one notification.",
    eventBackgroundTask: "Background task",
    eventWorkTask: "Work task",
  },
  NotificationSoundSettings: {
    title: "Notification sounds",
    description:
      "Play a short sound when an agent event fires. These are the same events the chat channels push — configured here for this device only.",
    enableHint:
      "Off by default. Sounds play in the workspace window of this browser or app only.",
    volume: "Volume",
    preview: "Preview",
    previewEvent: "Preview the {event} sound",
    onlyWhenUnfocused: "Only when the window is not focused",
    onlyWhenUnfocusedHint: "Stay silent while you are looking at Codeg.",
    eventsTitle: "Events",
    eventsHint:
      "Pick a tone per event, or “Silent” to skip it. Repeats of the same event within a couple of seconds play once.",
    toneNone: "Silent",
    toneChime: "Chime",
    toneDing: "Ding",
    toneBlip: "Blip",
    tonePop: "Pop",
    toneAlert: "Alert",
    toneDescend: "Descending",
  },
  ChatChannelSettings: {
    events: {
      title: "Event Notifications",
      description:
        "When enabled, triggered events will be pushed to the channel.",
      turnComplete: "Turn Complete",
      turnCompleteDesc: "When an agent turn ends",
      error: "Agent Error",
      errorDesc: "When an agent encounters an error",
      permissionRequest: "Permission Request",
      permissionRequestDesc: "When an agent requests permission to act",
      questionRequest: "Agent Question",
      questionRequestDesc: "When an agent asks you a question",
      userPromptSent: "User Message",
      userPromptSentDesc:
        "When you send a message — the message text is included in the notification",
      saved: "Event filter updated.",
      saveFailed: "Failed to save event filter.",
      loadFailed: "Failed to load settings.",
      retry: "Retry",
      webhooksTitle: "Webhooks",
      webhooksDescription:
        "POST a JSON payload to one or more URLs when an enabled event fires. The event filter above also applies to webhooks.",
      webhookUrlPlaceholder: "https://example.com/webhook",
      addWebhook: "Add Webhook",
      removeWebhook: "Remove webhook",
      webhookSave: "Save",
      webhooksSaved: "Webhooks saved.",
      webhooksSaveFailed: "Failed to save webhooks.",
      webhookInvalidUrl: "Enter valid http(s) URLs.",
      docsTitle: "Request Format",
      docsMethod: "Method",
      docsContentType: "Content-Type",
      docsNote:
        "Each enabled event is delivered to every URL. Webhooks are not debounced; the event filter above still applies.",
      editWebhook: "Edit Webhook",
      enableWebhook: "Enable webhook",
      webhookDuplicate: "This URL is already configured.",
      cancel: "Cancel",
      webhooksEmpty: "No webhooks configured yet.",
      deleteWebhookTitle: "Delete Webhook",
      deleteWebhookMessage:
        "Remove this webhook? Events will no longer be delivered to this URL.",
      delete: "Delete",
    },
  },
} as const

type AvailableTerminalShells = {
  options: Array<{
    id: string
    label_key: string
    value: string | null
    exists: boolean
    accepts_custom_path: boolean
  }>
}

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
    mockGetSettings.mockReset()
    mockGetShells.mockReset()
    mockUpdateSettings.mockReset()
    mockUpdateSettings.mockResolvedValue({ default_shell: null })
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

function renderSettings() {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <GeneralSettings />
    </NextIntlClientProvider>
  )
}

/**
 * The page is a stack of sections rendered through the shared
 * `SettingsSection` / `SettingCard` / `SettingRow` grammar, so what is worth
 * pinning is the wiring that grammar carries: every row's label resolves to the
 * control it names (a `SettingRow` with the `htmlFor` left off still looks
 * right and silently loses the association), and each section actually mounts.
 */
describe("GeneralSettings", () => {
  beforeEach(() => {
    platform.current = "windows"
    mockGetSettings.mockReset()
    mockGetShells.mockReset()
    mockUpdateSettings.mockReset()
    mockGetSettings.mockResolvedValue({ default_shell: null })
    mockGetShells.mockResolvedValue({
      resolved_shell: "/bin/zsh",
      effective_shell: "/bin/zsh",
      options: baseOptions,
    })
    mockUpdateSettings.mockImplementation(async (v: unknown) => v)
  })

  it("mounts every section and wires each row's label to its control", async () => {
    renderSettings()

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
      "Colorize command output",
      "Disable hardware acceleration",
      "Desktop notifications",
      "Notification sounds",
      "Multi-Agent Collaboration",
      "In-conversation tools",
      "Built-in browser",
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

  /**
   * The command-color switch ships OFF and has to stay that way — forcing
   * color on the agent breaks machine parsing of everything the agent runs, so
   * an accidental default flip is the regression worth catching. The save also
   * has to carry `default_shell` back unchanged: both settings share one stored
   * row, so a payload missing it would wipe the user's shell choice.
   */
  it("defaults command color off and preserves the shell when toggling it", async () => {
    renderSettings()

    const colorize = await screen.findByLabelText("Colorize command output")
    expect(colorize).toHaveAttribute("data-state", "unchecked")

    fireEvent.click(colorize)

    await waitFor(() =>
      expect(colorize).toHaveAttribute("data-state", "checked")
    )
    expect(vi.mocked(updateSystemTerminalSettings)).toHaveBeenCalledWith({
      default_shell: null,
      colorize_command_output: true,
    })
  })

  /**
   * A failed load is the one state where the switch must not be operable. The
   * save replaces the whole stored row, so a toggle made before the row was
   * read would send `default_shell: null` — indistinguishable from "the user
   * picked the system shell" — and quietly discard a configured shell path.
   * The picker is already inert here; the switch has to be too.
   */
  it("keeps the command-color switch inert when the settings fail to load", async () => {
    vi.mocked(updateSystemTerminalSettings).mockClear()
    vi.mocked(getSystemTerminalSettings).mockRejectedValueOnce(
      new Error("backend unreachable")
    )

    renderSettings()

    const colorize = await screen.findByLabelText("Colorize command output")
    expect(colorize).toBeDisabled()

    fireEvent.click(colorize)

    await waitFor(() =>
      expect(
        screen.getByText(/Load failed: backend unreachable/)
      ).toBeInTheDocument()
    )
    expect(vi.mocked(updateSystemTerminalSettings)).not.toHaveBeenCalled()
    expect(colorize).toHaveAttribute("data-state", "unchecked")
  })

  /**
   * The other end of the same coupling: once a shell save has landed, the
   * color toggle has to echo the NEW shell back. The save is followed by a
   * fallible options refresh, and a failure there used to leave the remembered
   * shell one revision behind — so the next toggle would faithfully resend the
   * superseded value and undo a save the user watched succeed.
   */
  it("keeps a just-saved shell when the options refresh fails", async () => {
    vi.mocked(updateSystemTerminalSettings).mockClear()
    // A stored path outside the option list renders the custom-path row, which
    // is the save route that needs no Select interaction.
    vi.mocked(getSystemTerminalSettings).mockResolvedValueOnce({
      default_shell: "/opt/fish",
      colorize_command_output: false,
    })

    renderSettings()

    const path = await screen.findByLabelText("Shell path")
    fireEvent.change(path, { target: { value: "/opt/fish2" } })

    // The save lands; only the refresh that follows it fails.
    vi.mocked(getAvailableTerminalShells).mockRejectedValueOnce(
      new Error("options unavailable")
    )
    // Several sections have a "Save"; this one sits in the input's own row.
    const shellRow = path.parentElement as HTMLElement
    fireEvent.click(within(shellRow).getByRole("button", { name: "Save" }))

    await waitFor(() =>
      expect(vi.mocked(updateSystemTerminalSettings)).toHaveBeenCalledWith({
        default_shell: "/opt/fish2",
        colorize_command_output: false,
      })
    )

    const colorize = screen.getByLabelText("Colorize command output")
    await waitFor(() => expect(colorize).toBeEnabled())
    fireEvent.click(colorize)

    await waitFor(() =>
      expect(vi.mocked(updateSystemTerminalSettings)).toHaveBeenLastCalledWith({
        default_shell: "/opt/fish2",
        colorize_command_output: true,
      })
    )
  })

  // The switch only means something where the backend has an env knob to flip
  // at startup: `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` on Windows,
  // `WEBKIT_DISABLE_*` on Linux. WKWebView has neither.
  it("offers the rendering toggle on Linux too", async () => {
    platform.current = "linux"
    renderSettings()

    await screen.findByLabelText("Default Terminal")
    expect(
      screen.getByLabelText("Disable hardware acceleration")
    ).toBeInTheDocument()
  })

  it("hides the rendering toggle on macOS", async () => {
    platform.current = "macos"
    renderSettings()

    await screen.findByLabelText("Default Terminal")
    expect(
      screen.queryByLabelText("Disable hardware acceleration")
    ).not.toBeInTheDocument()
  })
})
