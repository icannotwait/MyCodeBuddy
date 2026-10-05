import { render, screen } from "@testing-library/react"
import { describe, expect, it } from "vitest"

import { CreateRoundtable } from "@/components/roundtable/create-roundtable"
import { PreflightConfirmation } from "@/components/roundtable/preflight-confirmation"

describe("create roundtable", () => {
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
