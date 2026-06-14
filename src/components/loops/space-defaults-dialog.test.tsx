import { fireEvent, render, screen, waitFor } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

import { SpaceDefaultsDialog } from "./space-defaults-dialog"
import { defaultIssueConfig } from "@/lib/loop-config"

const stableT = (key: string) => key
vi.mock("next-intl", () => ({ useTranslations: () => stableT }))

vi.mock("sonner", () => ({ toast: { success: vi.fn(), error: vi.fn() } }))

const setLoopSpaceDefaultConfig = vi.fn().mockResolvedValue(undefined)
vi.mock("@/lib/loops-api", () => ({
  setLoopSpaceDefaultConfig: (...a: unknown[]) =>
    setLoopSpaceDefaultConfig(...a),
}))

// Stub the (separately tested) tabbed form, keep the real config↔form helpers.
vi.mock("./loop-config-form", async (orig) => {
  const real = await orig<typeof import("./loop-config-form")>()
  return {
    ...real,
    LoopConfigForm: () => <div data-testid="config-form" />,
  }
})

vi.mock("@/components/ui/dialog", () => ({
  Dialog: ({ open, children }: { open: boolean; children: React.ReactNode }) =>
    open ? <div>{children}</div> : null,
  DialogContent: ({ children }: { children: React.ReactNode }) => (
    <div>{children}</div>
  ),
  DialogHeader: ({ children }: { children: React.ReactNode }) => (
    <div>{children}</div>
  ),
  DialogFooter: ({ children }: { children: React.ReactNode }) => (
    <div>{children}</div>
  ),
  DialogTitle: ({ children }: { children: React.ReactNode }) => (
    <div>{children}</div>
  ),
  DialogDescription: ({ children }: { children: React.ReactNode }) => (
    <div>{children}</div>
  ),
}))

beforeEach(() => vi.clearAllMocks())

describe("SpaceDefaultsDialog", () => {
  it("saves the engine default when there is no current config", async () => {
    render(
      <SpaceDefaultsDialog
        spaceId={7}
        current={null}
        open
        onOpenChange={() => {}}
      />
    )
    fireEvent.click(screen.getByText("save"))
    await waitFor(() =>
      expect(setLoopSpaceDefaultConfig).toHaveBeenCalledWith(
        7,
        defaultIssueConfig()
      )
    )
  })

  it("clears the default (null) on reset", async () => {
    render(
      <SpaceDefaultsDialog
        spaceId={7}
        current={defaultIssueConfig()}
        open
        onOpenChange={() => {}}
      />
    )
    fireEvent.click(screen.getByText("resetToDefault"))
    await waitFor(() =>
      expect(setLoopSpaceDefaultConfig).toHaveBeenCalledWith(7, null)
    )
  })
})
