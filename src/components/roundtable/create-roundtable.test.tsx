import { fireEvent, render, screen } from "@testing-library/react"
import { describe, expect, it, vi } from "vitest"

import { CreateRoundtable } from "@/components/roundtable/create-roundtable"
import { PreflightConfirmation } from "@/components/roundtable/preflight-confirmation"
import messages from "@/i18n/messages/zh-CN.json"
vi.mock("next-intl", () => ({
  useTranslations: () => (key: keyof typeof messages.Roundtable) =>
    messages.Roundtable[key],
}))

describe("create roundtable", () => {
  it("dispatches confirmed start and leaves disabled execution unavailable", () => {
    const start = vi.fn()
    const { rerender } = render(
      <CreateRoundtable
        mode="parallel"
        confirmed
        onConfirm={() => undefined}
        onStart={start}
        enabled={false}
      />
    )
    fireEvent.click(screen.getByRole("button", { name: "开始" }))
    expect(start).not.toHaveBeenCalled()
    rerender(
      <CreateRoundtable
        mode="parallel"
        confirmed
        onConfirm={() => undefined}
        onStart={start}
        enabled
      />
    )
    fireEvent.click(screen.getByRole("button", { name: "开始" }))
    expect(start).toHaveBeenCalledOnce()
  })
  it("shows the consultation choice and holds start until confirmation", () => {
    render(
      <CreateRoundtable
        mode="parallel"
        quotaError="额度不足"
        confirmed={false}
        onConfirm={() => undefined}
      />
    )
    expect(screen.getByText("并行咨询")).toBeTruthy()
    expect(screen.getByRole("alert").textContent).toContain("额度不足")
    expect(screen.getAllByRole("button")[1].hasAttribute("disabled")).toBe(true)
    render(
      <PreflightConfirmation
        targets={["主持人", "成员"]}
        tools={["read"]}
        network="none"
        writes="none"
        budget="10"
      />
    )
    expect(screen.getByText("主持人、成员")).toBeTruthy()
    expect(screen.getByText("10")).toBeTruthy()
  })
})
