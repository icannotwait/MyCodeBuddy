import { fireEvent, render, screen, waitFor } from "@testing-library/react"
import { useState } from "react"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"
import enMessages from "@/i18n/messages/en.json"

const mocks = vi.hoisted(() => ({
  update: vi.fn(),
  toastError: vi.fn(),
}))

vi.mock("@/lib/api", () => ({
  acpUpdateAgentDisplayPreferences: mocks.update,
}))
vi.mock("sonner", () => ({
  toast: { error: mocks.toastError },
}))

import { AgentThinkingVisibilitySwitch } from "./agent-thinking-visibility-switch"

function Harness() {
  const [checked, setChecked] = useState(false)
  return (
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <output data-testid="value">{String(checked)}</output>
      <AgentThinkingVisibilitySwitch
        agentType="codex"
        checked={checked}
        onCheckedChange={(_, next) => setChecked(next)}
      />
    </NextIntlClientProvider>
  )
}

describe("AgentThinkingVisibilitySwitch", () => {
  beforeEach(() => {
    mocks.update.mockReset()
    mocks.toastError.mockReset()
  })

  it("updates optimistically and keeps the saved value", async () => {
    mocks.update.mockResolvedValue(undefined)
    render(<Harness />)
    fireEvent.click(screen.getByRole("switch", { name: "Show thinking" }))
    expect(screen.getByTestId("value")).toHaveTextContent("true")
    await waitFor(() => {
      expect(mocks.update).toHaveBeenCalledWith("codex", true)
      expect(
        screen.getByRole("switch", { name: "Show thinking" })
      ).toBeEnabled()
    })
    expect(screen.getByTestId("value")).toHaveTextContent("true")
  })

  it("rolls back and reports a failed save", async () => {
    mocks.update.mockRejectedValue(new Error("disk full"))
    render(<Harness />)
    fireEvent.click(screen.getByRole("switch", { name: "Show thinking" }))
    expect(screen.getByTestId("value")).toHaveTextContent("true")
    await waitFor(() => {
      expect(screen.getByTestId("value")).toHaveTextContent("false")
      expect(mocks.toastError).toHaveBeenCalledTimes(1)
    })
  })
})
