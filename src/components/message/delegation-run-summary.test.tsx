import { render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"

import { DelegationRunSummary } from "@/components/message/delegation-run-summary"
import enMessages from "@/i18n/messages/en.json"

function renderSummary() {
  render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <DelegationRunSummary
        summary={{
          kind: "author",
          status: "done",
          summary: "Plan is ready.",
          plan_digest: "sha256:plan-v2",
          report_file: "docs/superpowers/plans/adaptive-routing.md",
        }}
      />
    </NextIntlClientProvider>
  )
}

describe("DelegationRunSummary", () => {
  it("renders an Author summary as Plan evidence", () => {
    renderSummary()

    expect(screen.getByText("Plan: Done")).toBeInTheDocument()
    expect(screen.getByText("Plan is ready.")).toBeInTheDocument()
    expect(screen.queryByText("Delivery: Done")).not.toBeInTheDocument()
  })
})
