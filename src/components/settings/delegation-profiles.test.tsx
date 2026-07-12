import { fireEvent, render, screen, within } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { describe, expect, it, vi } from "vitest"

import enMessages from "@/i18n/messages/en.json"

vi.mock("@/lib/api", () => ({
  describeAgentOptions: vi.fn().mockResolvedValue({
    modes: null,
    config_options: [],
    available_commands: [],
  }),
}))

import { DelegationProfilesPanel } from "./delegation-profiles"

describe("DelegationProfilesPanel", () => {
  it("creates a CodeBuddy profile by copying delegation defaults", () => {
    const onChange = vi.fn()
    render(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <DelegationProfilesPanel
          value={[]}
          codeBuddyDefaults={{
            mode_id: "accept_edits",
            config_values: { model: "opus-4.8", permission: "strict" },
          }}
          onChange={onChange}
        />
      </NextIntlClientProvider>
    )

    fireEvent.click(screen.getByRole("button", { name: "Add profile" }))

    const profiles = onChange.mock.calls[0][0]
    expect(profiles).toHaveLength(1)
    expect(profiles[0]).toMatchObject({
      agent_type: "code_buddy",
      name: "New profile",
      mode_id: "accept_edits",
      config_values: { model: "opus-4.8", permission: "strict" },
      enabled: true,
    })
    expect(profiles[0].id).toBeTruthy()
  })

  it("requires confirmation before deleting a profile", async () => {
    const onChange = vi.fn()
    render(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <DelegationProfilesPanel
          value={[
            {
              id: "11111111-1111-4111-8111-111111111111",
              agent_type: "code_buddy",
              name: "GLM5.2",
              config_values: { model: "glm-5.2" },
              enabled: true,
              created_at: 1,
              updated_at: 1,
            },
          ]}
          codeBuddyDefaults={{ config_values: {} }}
          onChange={onChange}
        />
      </NextIntlClientProvider>
    )

    await screen.findByText("This agent has no configurable options.")
    fireEvent.click(screen.getByRole("button", { name: "Delete profile" }))

    expect(onChange).not.toHaveBeenCalled()
    const dialog = screen.getByRole("alertdialog")
    fireEvent.click(
      within(dialog).getByRole("button", { name: "Delete profile" })
    )
    expect(onChange).toHaveBeenCalledWith([])
  })
})
