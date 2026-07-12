import { fireEvent, render, screen, waitFor } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"
import { toast } from "sonner"

vi.mock("@/lib/api", () => ({
  getDelegationSettings: vi.fn(),
  setDelegationSettings: vi.fn(),
  getDelegationProfiles: vi.fn(),
  setDelegationProfiles: vi.fn(),
  setDelegationBundle: vi.fn(),
  describeAgentOptions: vi.fn(),
}))

vi.mock("sonner", () => ({
  toast: {
    success: vi.fn(),
    error: vi.fn(),
  },
}))

import { DelegationSettingsSection } from "./delegation-settings"
import enMessages from "@/i18n/messages/en.json"
import {
  getDelegationSettings,
  setDelegationBundle,
  getDelegationProfiles,
  type DelegationSettings,
} from "@/lib/api"

const mockGetDelegationSettings = vi.mocked(getDelegationSettings)
const mockSetDelegationBundle = vi.mocked(setDelegationBundle)
const mockGetDelegationProfiles = vi.mocked(getDelegationProfiles)
const mockToastError = vi.mocked(toast.error)

function renderWithIntl() {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <DelegationSettingsSection />
    </NextIntlClientProvider>
  )
}

function settings(
  overrides: Partial<DelegationSettings> = {}
): DelegationSettings {
  // Mirror the new backend default (DelegationSettings::default) so tests
  // that don't care about the toggle reflect the production wire shape.
  // Tests that need delegation active for save/depth assertions must
  // override explicitly.
  return {
    enabled: false,
    depth_limit: 1,
    completed_cache_max_mb: 512,
    agent_defaults: {},
    ...overrides,
  }
}

beforeEach(() => {
  mockGetDelegationSettings.mockReset()
  mockSetDelegationBundle.mockReset().mockImplementation(async (bundle) => ({
    settings: bundle.settings,
    profiles: bundle.profiles,
  }))
  mockGetDelegationProfiles.mockReset().mockResolvedValue({ profiles: [] })
  mockToastError.mockReset()
})

describe("DelegationSettingsSection", () => {
  it("renders the enable switch and depth input", async () => {
    mockGetDelegationSettings.mockResolvedValue(settings())

    renderWithIntl()

    expect(
      await screen.findByLabelText("Maximum delegation depth")
    ).toBeInTheDocument()
    expect(screen.getByLabelText("Enable delegation")).toBeInTheDocument()
    expect(
      screen.getByLabelText("Completed-result cache (MB)")
    ).toBeInTheDocument()
    // No timeout knob anymore — cancel flows through MCP notifications.
    expect(screen.queryByLabelText(/timeout/i)).not.toBeInTheDocument()
  })

  it("saves the completed-result cache budget (MB); 0 means unlimited", async () => {
    mockGetDelegationSettings.mockResolvedValue(settings({ enabled: true }))

    renderWithIntl()

    const cacheInput = await screen.findByLabelText(
      "Completed-result cache (MB)"
    )
    fireEvent.change(cacheInput, { target: { value: "0" } })
    fireEvent.click(screen.getByRole("button", { name: "Save" }))

    await waitFor(() => {
      expect(mockSetDelegationBundle).toHaveBeenCalledWith({
        settings: {
          enabled: true,
          depth_limit: 1,
          completed_cache_max_mb: 0,
          agent_defaults: {},
        },
        profiles: { profiles: [] },
      })
    })
  })

  it("clearing the cache input falls back to the default, not unlimited", async () => {
    mockGetDelegationSettings.mockResolvedValue(
      settings({ enabled: true, completed_cache_max_mb: 256 })
    )

    renderWithIntl()

    const cacheInput = await screen.findByLabelText(
      "Completed-result cache (MB)"
    )
    fireEvent.change(cacheInput, { target: { value: "" } })
    fireEvent.click(screen.getByRole("button", { name: "Save" }))

    await waitFor(() => {
      expect(mockSetDelegationBundle).toHaveBeenCalledWith({
        settings: {
          enabled: true,
          depth_limit: 1,
          completed_cache_max_mb: 512,
          agent_defaults: {},
        },
        profiles: { profiles: [] },
      })
    })
  })

  it("saves the depth_limit and enabled flag", async () => {
    // Depth input is disabled while `enabled` is false (the production
    // default), so this flow explicitly opts in.
    mockGetDelegationSettings.mockResolvedValue(settings({ enabled: true }))

    renderWithIntl()

    const depthInput = await screen.findByLabelText("Maximum delegation depth")
    fireEvent.change(depthInput, { target: { value: "5" } })
    fireEvent.click(screen.getByRole("button", { name: "Save" }))

    await waitFor(() => {
      expect(mockSetDelegationBundle).toHaveBeenCalledWith({
        settings: {
          enabled: true,
          depth_limit: 5,
          completed_cache_max_mb: 512,
          agent_defaults: {},
        },
        profiles: { profiles: [] },
      })
    })
  })

  it("reflects backend default (disabled): switch off, depth input disabled", async () => {
    // Regression for the "default off" UX guarantee: when persistence has
    // never been written, the backend returns `enabled: false` and the
    // panel must surface that. Switch un-checked + depth input disabled
    // is what blocks the user from changing depth/agent-defaults before
    // they consciously opt in.
    mockGetDelegationSettings.mockResolvedValue(settings())

    renderWithIntl()

    const depthInput = (await screen.findByLabelText(
      "Maximum delegation depth"
    )) as HTMLInputElement
    const enableSwitch = screen.getByLabelText(
      "Enable delegation"
    ) as HTMLButtonElement

    expect(enableSwitch).toHaveAttribute("data-state", "unchecked")
    expect(depthInput).toBeDisabled()
  })

  it("toasts saveFailed and loadFailed when save and post-failure reload both fail", async () => {
    // Mount load succeeds; save fails; post-save resync also fails.
    mockGetDelegationSettings
      .mockResolvedValueOnce(settings({ enabled: true }))
      .mockRejectedValueOnce(new Error("reload boom"))
    mockGetDelegationProfiles
      .mockResolvedValueOnce({ profiles: [] })
      .mockRejectedValueOnce(new Error("reload boom"))
    mockSetDelegationBundle.mockRejectedValueOnce(new Error("persist boom"))

    renderWithIntl()
    await screen.findByLabelText("Maximum delegation depth")

    fireEvent.click(screen.getByRole("button", { name: "Save" }))

    await waitFor(() => {
      expect(mockToastError).toHaveBeenCalledWith(
        "Failed to save delegation settings",
        expect.objectContaining({ description: "persist boom" })
      )
    })
    await waitFor(() => {
      expect(mockToastError).toHaveBeenCalledWith(
        "Failed to load delegation settings: reload boom"
      )
    })
  })
})
