import { fireEvent, render, screen, waitFor } from "@testing-library/react"
import { useState } from "react"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"
import enMessages from "@/i18n/messages/en.json"
import type { AgentType } from "@/lib/types"

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

function MultiAgentHarness({ agentType }: { agentType: AgentType }) {
  const [checkedByAgent, setCheckedByAgent] = useState<
    Partial<Record<AgentType, boolean>>
  >({})
  const checked = checkedByAgent[agentType] ?? false
  return (
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <output data-testid="value">{String(checked)}</output>
      <output data-testid="agent">{agentType}</output>
      <AgentThinkingVisibilitySwitch
        agentType={agentType}
        checked={checked}
        onCheckedChange={(type, next) =>
          setCheckedByAgent((prev) => ({ ...prev, [type]: next }))
        }
      />
    </NextIntlClientProvider>
  )
}

function OverwriteHarness() {
  const [checked, setChecked] = useState(false)
  return (
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <output data-testid="value">{String(checked)}</output>
      <button
        type="button"
        data-testid="overwrite"
        onClick={() => setChecked(false)}
      >
        overwrite
      </button>
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

  it("tracks in-flight save per agent so other agents stay enabled", async () => {
    let resolveSave!: () => void
    mocks.update.mockImplementation(
      () =>
        new Promise<void>((resolve) => {
          resolveSave = () => resolve()
        })
    )

    const { rerender } = render(<MultiAgentHarness agentType="codex" />)

    fireEvent.click(screen.getByRole("switch", { name: "Show thinking" }))
    expect(screen.getByRole("switch", { name: "Show thinking" })).toBeDisabled()
    expect(screen.getByTestId("value")).toHaveTextContent("true")

    rerender(<MultiAgentHarness agentType="gemini" />)
    expect(screen.getByTestId("agent")).toHaveTextContent("gemini")
    expect(screen.getByRole("switch", { name: "Show thinking" })).toBeEnabled()

    rerender(<MultiAgentHarness agentType="codex" />)
    expect(screen.getByTestId("agent")).toHaveTextContent("codex")
    expect(screen.getByRole("switch", { name: "Show thinking" })).toBeDisabled()

    resolveSave()
    await waitFor(() => {
      expect(
        screen.getByRole("switch", { name: "Show thinking" })
      ).toBeEnabled()
    })
  })

  it("re-applies saved value after parent overwrites mid-flight", async () => {
    let resolveSave!: () => void
    mocks.update.mockImplementation(
      () =>
        new Promise<void>((resolve) => {
          resolveSave = () => resolve()
        })
    )

    render(<OverwriteHarness />)
    fireEvent.click(screen.getByRole("switch", { name: "Show thinking" }))
    expect(screen.getByTestId("value")).toHaveTextContent("true")

    fireEvent.click(screen.getByTestId("overwrite"))
    expect(screen.getByTestId("value")).toHaveTextContent("false")

    resolveSave()
    await waitFor(() => {
      expect(screen.getByTestId("value")).toHaveTextContent("true")
      expect(
        screen.getByRole("switch", { name: "Show thinking" })
      ).toBeEnabled()
    })
  })

  it("blocks duplicate in-flight saves for the same agent", async () => {
    let resolveSave!: () => void
    mocks.update.mockImplementation(
      () =>
        new Promise<void>((resolve) => {
          resolveSave = () => resolve()
        })
    )

    render(<Harness />)
    const sw = screen.getByRole("switch", { name: "Show thinking" })
    fireEvent.click(sw)
    fireEvent.click(sw)
    expect(mocks.update).toHaveBeenCalledTimes(1)

    resolveSave()
    await waitFor(() => {
      expect(sw).toBeEnabled()
    })
  })
})
