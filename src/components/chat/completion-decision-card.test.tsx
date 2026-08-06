import { render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"

import enMessages from "@/i18n/messages/en.json"
import type { CompletionProjectionV2 } from "@/lib/types"
import { CompletionDecisionCard } from "./completion-decision-card"
import {
  resolveCompletionDecision,
  retryCompletionArtifact,
} from "@/lib/api"

vi.mock("@/lib/api", async () => {
  const actual = await vi.importActual<typeof import("@/lib/api")>("@/lib/api")
  return {
    ...actual,
    resolveCompletionDecision: vi.fn(),
    retryCompletionArtifact: vi.fn(),
  }
})

const cas = {
  attention_id: "attention-1",
  task_id: "task-1",
  kind: "completion_decision" as const,
  captured_scope_digest: `sha256:${"a".repeat(64)}`,
  latest_run_id: "task-1",
  node_id: "plan-reviewer",
}

function projection(
  overrides: Partial<CompletionProjectionV2> = {}
): CompletionProjectionV2 {
  return {
    protocol_version: 2,
    graph_revision: 7,
    card: {
      state: "needs_decision",
      role: "reviewer",
      outcome: null,
      summary: "Choose the durable reviewer outcome.",
      report_file: null,
      source: null,
      evidence_validated: false,
      attention: cas,
    },
    ...overrides,
  }
}

function renderCard(request = projection()) {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <CompletionDecisionCard request={request} />
    </NextIntlClientProvider>
  )
}

describe("CompletionDecisionCard", () => {
  beforeEach(() => {
    vi.mocked(resolveCompletionDecision).mockReset()
    vi.mocked(retryCompletionArtifact).mockReset()
  })

  it("submits a legal typed outcome with the exact six-field CAS", async () => {
    vi.mocked(resolveCompletionDecision).mockResolvedValue({
      completion: projection({
        graph_revision: 8,
        card: {
          ...projection().card,
          state: "resolved",
          outcome: "approve_with_minors",
          source: "user_adjudication",
          evidence_validated: true,
          attention: null,
        },
      }),
      graph_revision: 8,
    })
    renderCard()

    await userEvent.click(
      screen.getByRole("button", { name: "Approve with minors" })
    )

    expect(resolveCompletionDecision).toHaveBeenCalledWith({
      cas,
      outcome: "approve_with_minors",
    })
    expect(await screen.findByText("Resolved")).toBeInTheDocument()
  })

  it("keeps the selected choice after a retryable failure", async () => {
    vi.mocked(resolveCompletionDecision).mockRejectedValue(
      new Error("transport unavailable")
    )
    renderCard()

    const choice = screen.getByRole("button", { name: "Request changes" })
    await userEvent.click(choice)

    await waitFor(() => expect(choice).toHaveAttribute("aria-pressed", "true"))
    expect(screen.getByText("transport unavailable")).toBeInTheDocument()
  })

  it("shows artifact retry only for artifact recovery", async () => {
    renderCard(
      projection({
        card: {
          ...projection().card,
          state: "blocked",
          attention: { ...cas, kind: "completion_artifact_recovery" },
        },
      })
    )

    await userEvent.click(screen.getByRole("button", { name: "Retry artifact" }))
    expect(retryCompletionArtifact).toHaveBeenCalledWith({ cas: {
      ...cas,
      kind: "completion_artifact_recovery",
    } })
  })
})
